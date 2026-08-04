// The user-scope install runner (proposal 0050). [0046] made a CLI's *presence*
// a first-class concern and [0049] made *staying current* one; this module makes
// **becoming present** an action — from the update dialog, the dashboard row, or
// `doctor --install --yes` — without an SSH session, without a TTY, and without
// ever leaving `$HOME`.
//
// Three rules hold the whole thing up:
//
//   1. **Local-user scope, always.** Everything lands under `$HOME`
//      (`~/.local/bin`, `~/.local/share/…`). No `sudo`, no system package
//      manager, no write outside the home directory.
//   2. **"Installed" means launchable.** The verdict is `binary_on_path(bin,
//      env_path)` against the **session PATH** the engine spawns with — not the
//      installer's exit code, which is routinely 0 having achieved nothing. This
//      is `doctor`'s probe-decides-the-verdict discipline, and it is why a
//      "successful" install that left the binary off the PATH reports `failed`.
//   3. **Prerequisites are declared, not assumed.** `codex`/`gemini` need `npm`;
//      `kimi` needs `uv`. Each is installed user-locally, only when actually
//      missing, and only as a dependency of an assistant the user asked for.
//
// The command strings all live in `tools::{ASSISTANTS, PREREQS}` — this module
// never hard-codes a vendor command, so a stale one is a one-line registry fix.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cc_screen_protocol::{InstallPlan, InstallPlanItem, InstallPrereqPlan};

use crate::assistants;
use crate::tools::{self, Prereq, Tool};

/// Per-command wall clock for an *install*. Deliberately much longer than
/// [0049]'s 300 s update budget: a Node tarball plus an `npm i -g` on a cold
/// cache plus `uv` fetching a CPython build is genuinely minutes of work (the
/// `node` binary alone is 117 MB). Override with `CCWEB_INSTALL_TIMEOUT_SECS`.
const DEFAULT_INSTALL_TIMEOUT_SECS: u64 = 900;

/// A `npm prefix -g` answers immediately or it's broken.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// The outcome of installing ONE tool. `Installed` is claimed only when the
/// binary resolves on the **session** PATH afterwards.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallOutcome {
    /// It resolves on the session PATH now. `via` names how (the command, plus
    /// the landing-zone link when one was needed).
    Installed { version: String, via: String },
    /// Already there — installing is never re-run over a working CLI.
    AlreadyPresent { version: String },
    /// The installer's own error text, or "finished but still not launchable".
    Failed { error: String },
    /// No install command for this tool on this platform → the docs link.
    Unsupported { reason: String },
}

fn install_timeout() -> Duration {
    let secs = std::env::var("CCWEB_INSTALL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_INSTALL_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

// ── The install environment (Part B2) ────────────────────────────────────────

/// The environment an install command runs in — the user-scope guarantee, made
/// of variables set on the child only so the user's own npm/uv config is never
/// rewritten.
pub struct InstallEnv {
    /// The session PATH plus the dirs a fresh prerequisite just populated, so
    /// `npm` is usable in the same job that installed Node.
    pub path: String,
    pub vars: Vec<(&'static str, String)>,
    /// `$HOME/.local/bin` — the one landing zone `config::build_env_path`
    /// guarantees is on the session PATH, with no agent restart.
    pub local_bin: PathBuf,
    /// Where a user-scope installer is known to drop binaries; searched by the
    /// landing-zone step when the session PATH still doesn't resolve the binary.
    pub search: Vec<PathBuf>,
}

/// Whether we can actually create files in `dir` — the honest test, since a
/// Unix mode check can't answer it for the current uid.
fn writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".cc-screen-write-test");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// The npm prefix to install into: the user's **existing** `npm prefix -g` when
/// it is writable (so an `nvm`/`fnm`/`volta` layout is respected), else
/// `$HOME/.local/share/node`. `None` on Windows — the default prefix there
/// (`%APPDATA%\npm`) is already user-writable *and* already on the User PATH, so
/// the right move is to override nothing (both verified on `harebell`).
fn npm_prefix(env_path: &str, home: &Path) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    let run = assistants::run_shell("npm prefix -g", env_path, PROBE_TIMEOUT, &[]);
    if run.ok {
        if let Some(line) = run.output.lines().map(str::trim).find(|l| !l.is_empty()) {
            let p = PathBuf::from(line);
            if writable(&p) {
                return Some(p);
            }
        }
    }
    let fallback = home.join(".local").join("share").join("node");
    let _ = std::fs::create_dir_all(&fallback);
    Some(fallback)
}

/// Build the environment (and the landing-zone search list) for this machine.
pub fn install_env(env_path: &str, home: &Path) -> InstallEnv {
    let local_bin = home.join(".local").join("bin");
    let _ = std::fs::create_dir_all(&local_bin);
    let node_dir = home.join(".local").join("share").join("node");
    let prefix = npm_prefix(env_path, home);

    let mut vars: Vec<(&'static str, String)> = vec![
        // An unattended install must not wait on a prompt, nor spend a minute on
        // advisories nobody is reading.
        ("CI", "1".into()),
        ("npm_config_yes", "true".into()),
        ("npm_config_fund", "false".into()),
        ("npm_config_audit", "false".into()),
        ("npm_config_progress", "false".into()),
        // Be explicit about where uv's shims and uv itself go — and never let an
        // installer rewrite the user's PATH: a change that wouldn't reach the
        // running agent anyway (`config::load()` runs once at startup).
        ("UV_TOOL_BIN_DIR", local_bin.to_string_lossy().into_owned()),
        ("UV_INSTALL_DIR", local_bin.to_string_lossy().into_owned()),
        ("UV_NO_MODIFY_PATH", "1".into()),
    ];
    if let Some(p) = &prefix {
        vars.push(("npm_config_prefix", p.to_string_lossy().into_owned()));
    }

    // The install PATH: the session PATH, plus the bin dirs a prerequisite we
    // just installed populates. Prepended so a fresh `npm` wins over none.
    let sep = tools::PATH_SEP;
    let mut head: Vec<String> = Vec::new();
    for d in [Some(local_bin.clone()), prefix.as_ref().map(|p| p.join("bin")), Some(node_dir.join("bin"))]
        .into_iter()
        .flatten()
    {
        let s = d.to_string_lossy().into_owned();
        if !head.contains(&s) {
            head.push(s);
        }
    }
    let path = head
        .iter()
        .cloned()
        .chain(env_path.split(sep).map(str::to_string).filter(|s| !s.is_empty() && !head.contains(s)))
        .collect::<Vec<_>>()
        .join(&sep.to_string());

    let mut search: Vec<PathBuf> = Vec::new();
    for d in [
        prefix.as_ref().map(|p| p.join("bin")),
        // Windows npm puts the shims in the prefix ROOT, not prefix/bin.
        prefix.clone(),
        Some(node_dir.join("bin")),
        Some(local_bin.clone()),
        Some(home.join(".local").join("share").join("uv").join("tools")),
    ]
    .into_iter()
    .flatten()
    {
        if !search.contains(&d) {
            search.push(d);
        }
    }
    // Anything else a user-scope installer laid down under ~/.local/share/*/bin.
    if let Ok(rd) = std::fs::read_dir(home.join(".local").join("share")) {
        for e in rd.flatten() {
            let b = e.path().join("bin");
            if b.is_dir() && !search.contains(&b) {
                search.push(b);
            }
        }
    }

    InstallEnv { path, vars, local_bin, search }
}

// ── The landing zone (Part B4) ───────────────────────────────────────────────

/// Everything under `$HOME`, always — the same posture `confine.rs` takes for
/// file ops. An install action has no business writing outside the home dir.
fn under_home(p: &Path, home: &Path) -> bool {
    p.starts_with(home)
}

/// Find `bin` in the places a user-scope installer puts things and expose it in
/// `$HOME/.local/bin`, which `config::build_env_path` guarantees is first on the
/// session PATH — so `installed` means *launchable now*, with no new shell and
/// no agent restart. (Extending the agent's `env_path` instead would need a
/// restart, which is exactly what an install shouldn't require.)
///
/// Returns the path it exposed, `Ok(None)` when the binary wasn't found in any
/// known location, and `Err` when something is in the way.
pub fn land_in_local_bin(bin: &str, env: &InstallEnv, home: &Path) -> Result<Option<PathBuf>, String> {
    let joined = env
        .search
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(&tools::PATH_SEP.to_string());
    let Some(src) = tools::Resolver::host().resolve(bin, &joined) else { return Ok(None) };
    if !under_home(&src, home) {
        return Err(format!("`{bin}` was installed outside your home directory ({}) — left alone", src.display()));
    }
    let dst = env.local_bin.join(src.file_name().unwrap_or_default());
    if !under_home(&dst, home) {
        return Err(format!("landing zone {} is outside your home directory", dst.display()));
    }
    if src == dst {
        return Ok(Some(dst));
    }
    expose(&src, &dst)?;
    // On Windows an npm-installed CLI is a SET of shims (`codex`, `codex.cmd`,
    // `codex.ps1`); copying only the one the resolver picked would leave the
    // others behind and confuse a later `cmd.exe` lookup.
    if cfg!(windows) {
        if let Some(dir) = src.parent() {
            for ext in ["", ".cmd", ".ps1", ".bat", ".exe"] {
                let sibling = dir.join(format!("{bin}{ext}"));
                if sibling != src && sibling.is_file() {
                    let _ = expose(&sibling, &env.local_bin.join(format!("{bin}{ext}")));
                }
            }
        }
    }
    Ok(Some(dst))
}

/// Expose `src` at `dst`: a symlink on Unix, a **copy** on Windows (creating a
/// symlink there needs admin or Developer Mode, and this feature never asks for
/// privilege). Never clobbers something we didn't put there.
fn expose(src: &Path, dst: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(dst) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                // A link we could have made — already right, or repoint it.
                if std::fs::read_link(dst).ok().as_deref() == Some(src) {
                    return Ok(());
                }
                std::fs::remove_file(dst).map_err(|e| format!("could not replace {}: {e}", dst.display()))?;
            } else {
                // Anything we can't prove we put there stays. An install action
                // must not overwrite a binary the user placed in their own
                // ~/.local/bin. (Rarely reached: a file that's already there
                // would make the binary resolve, so we'd never be landing it.)
                return Err(format!(
                    "{} already exists and isn't ours — left alone (move it aside and retry)",
                    dst.display()
                ));
            }
        }
        Err(_) => {} // nothing there
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, dst)
            .map_err(|e| format!("could not link {} → {}: {e}", dst.display(), src.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| format!("could not copy {} → {}: {e}", src.display(), dst.display()))
    }
}

// ── Prerequisites (Part B1 step 3) ───────────────────────────────────────────

/// Make a prerequisite available, or say why not. A prerequisite that fails ends
/// the assistant's row — we never run an install command whose toolchain we know
/// is absent, because its error (`npm: not found`) would be the confusing one
/// instead of the true one.
pub fn ensure_prereq(p: &'static Prereq, env: &InstallEnv, home: &Path) -> Result<(), String> {
    if tools::binary_on_path(p.bin, &env.path) {
        return Ok(());
    }
    let Some(cmd) = tools::prereq_install_command(p) else {
        return Err(format!("{} isn't installed and there's no user-scope installer for it here — see {}", p.label, p.docs));
    };
    let run = assistants::run_shell(cmd, &env.path, install_timeout(), &env.vars);
    for b in p.also_link {
        let _ = land_in_local_bin(b, env, home);
    }
    if tools::binary_on_path(p.bin, &env.path) {
        return Ok(());
    }
    Err(assistants::tail_error(
        &run.output,
        &format!("installing {} failed", p.label),
    ))
}

// ── Installing one tool (Part B1) ────────────────────────────────────────────

/// Install `t` for the local user and report what actually happened.
///
/// 1. **Probe first** — present → `AlreadyPresent`; a working CLI is never
///    re-installed over.
/// 2. **Resolve the command** (`tools::install_command`): a `cc_tool_install`
///    override wins, else the registry's per-OS line, else `Unsupported`.
/// 3. **Satisfy prerequisites** — skipped entirely when the machine declared its
///    own command; the self-hoster said what they wanted.
/// 4. **Run it** with the user-scope environment and the install timeout.
/// 5. **Land it, then re-probe.** The probe against the *session* PATH is the
///    verdict — an installer that exits 0 and leaves nothing launchable is a
///    `Failed` row, not a shrug.
pub fn install_tool(t: &Tool, env_path: &str, home: &Path) -> InstallOutcome {
    let Some(bin) = tools::probe_binary(t).map(str::to_string) else {
        return InstallOutcome::Unsupported { reason: "nothing to install (no binary to probe)".into() };
    };
    if tools::binary_on_path(&bin, env_path) {
        return InstallOutcome::AlreadyPresent { version: assistants::probe_version(t, env_path) };
    }
    let Some(cmd) = tools::install_command(t) else {
        let docs = tools::docs_url(t)
            .map(|d| format!("see {d}"))
            .unwrap_or_else(|| "declare one with cc_tool_install".to_string());
        return InstallOutcome::Unsupported {
            reason: format!("no install command for this platform — {docs}"),
        };
    };

    let env = install_env(env_path, home);
    for p in tools::prereqs_for(t) {
        if let Err(e) = ensure_prereq(p, &env, home) {
            return InstallOutcome::Failed { error: format!("needs {}: {e}", p.label) };
        }
    }

    let run = assistants::run_shell(&cmd, &env.path, install_timeout(), &env.vars);

    // Land it: an install that dropped the binary into a prefix the session PATH
    // doesn't include is the common case (a human does this by hand today).
    let mut via = format!("`{cmd}`");
    if !tools::binary_on_path(&bin, env_path) {
        match land_in_local_bin(&bin, &env, home) {
            Ok(Some(p)) => via = format!("{via} → {}", p.display()),
            Ok(None) => {}
            Err(e) => return InstallOutcome::Failed { error: e },
        }
    }

    if tools::binary_on_path(&bin, env_path) {
        return InstallOutcome::Installed { version: assistants::probe_version(t, env_path), via };
    }
    // Not launchable. If the command itself failed, its own text is the truth;
    // if it exited 0, the PATH is the story (doctor.rs said this as a shrug —
    // here it's a failure, because the row claims nothing it can't back up).
    let error = if run.ok {
        format!("`{cmd}` finished, but `{bin}` still isn't on the session PATH")
    } else {
        assistants::tail_error(&run.output, run.error.as_deref().unwrap_or("install failed"))
    };
    InstallOutcome::Failed { error }
}

// ── The plan (Part B, consumed by the dialog before anything runs) ───────────

/// What installing `t` *would* do: the exact command, the prerequisites that are
/// actually missing, and the size hints. A pure probe — no side effects — so the
/// UI never hard-codes a vendor command and the registry stays the one source of
/// truth. `None` when the tool is already installed (nothing to plan).
pub fn plan_tool(t: &Tool, env_path: &str) -> Option<InstallPlanItem> {
    let bin = tools::probe_binary(t)?;
    if tools::binary_on_path(bin, env_path) {
        return None;
    }
    let command = tools::install_command(t).unwrap_or_default();
    let prereqs = tools::prereqs_for(t)
        .into_iter()
        .filter(|p| !tools::binary_on_path(p.bin, env_path))
        .map(|p| InstallPrereqPlan {
            key: p.key.to_string(),
            label: p.label.to_string(),
            command: tools::prereq_install_command(p).unwrap_or_default().to_string(),
            docs: p.docs.to_string(),
            size_hint: tools::prereq_size_hint(p).to_string(),
        })
        .collect::<Vec<_>>();
    // Honest up front: a prerequisite with no user-scope bootstrap here (Node on
    // Windows) makes the whole row unsupported, rather than promising an install
    // that would fail minutes later.
    let unsupported = if command.is_empty() {
        Some(format!(
            "no install command for this platform{}",
            tools::docs_url(t).map(|d| format!(" — see {d}")).unwrap_or_default()
        ))
    } else {
        prereqs
            .iter()
            .find(|p| p.command.is_empty())
            .map(|p| format!("{} must be installed first — see {}", p.label, p.docs))
    };
    Some(InstallPlanItem {
        tool: t.prefix.clone(),
        label: tools::label(t),
        command,
        docs: tools::docs_url(t).unwrap_or_default().to_string(),
        size_hint: tools::size_hint(t).to_string(),
        prereqs,
        unsupported,
    })
}

/// The plan for every registered tool that is missing, narrowed to `wanted`
/// (by prefix or cmd) when that isn't empty.
pub fn plan(tools_list: &[Tool], env_path: &str, wanted: &[String]) -> InstallPlan {
    InstallPlan {
        items: tools_list
            .iter()
            .filter(|t| wanted.is_empty() || wanted.iter().any(|w| w == &t.prefix || w == &t.cmd))
            .filter_map(|t| plan_tool(t, env_path))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(prefix: &str, tmpl: &str) -> Tool {
        Tool {
            cmd: format!("{prefix}-cmd"),
            prefix: prefix.to_string(),
            tmpl: tmpl.to_string(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
            install_hint: None,
            update_cmd: None,
        }
    }

    /// A fake `$HOME` plus a scratch dir of fake installers on the PATH. The
    /// *session* PATH deliberately contains only `$HOME/.local/bin` and the
    /// installer dir — so a fake that drops a binary anywhere else is only
    /// launchable if the landing-zone step did its job.
    struct Box_ {
        home: PathBuf,
        bins: PathBuf,
    }
    impl Box_ {
        fn new(tag: &str) -> Box_ {
            let root = std::env::temp_dir().join(format!("ccr-prov-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            let bins = root.join("bins");
            std::fs::create_dir_all(home.join(".local").join("bin")).unwrap();
            std::fs::create_dir_all(&bins).unwrap();
            Box_ { home, bins }
        }
        fn write(&self, name: &str, script: &str) {
            let p = self.bins.join(name);
            std::fs::write(&p, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        /// The session PATH: ~/.local/bin first (as `build_env_path` guarantees),
        /// then the fake-installer dir, then just the system coreutils.
        /// Deliberately NOT the developer's real PATH — this box must look like a
        /// machine with none of the assistants installed, and the test host very
        /// likely has all four.
        fn env_path(&self) -> String {
            format!(
                "{}:{}:/usr/bin:/bin",
                self.home.join(".local").join("bin").display(),
                self.bins.display(),
            )
        }
    }
    impl Drop for Box_ {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_landing_zone_is_what_makes_installed_mean_launchable() {
        // The crux of Part B4: the installer succeeds, drops the binary in a
        // prefix the session PATH does NOT contain, and the row passes only
        // because we linked it into ~/.local/bin.
        let b = Box_::new("landing");
        let prefix_bin = b.home.join(".local").join("share").join("node").join("bin");
        std::fs::create_dir_all(&prefix_bin).unwrap();
        b.write(
            "fake-install",
            &format!(
                "#!/bin/sh\nprintf '#!/bin/sh\\necho \"newcli 1.2.3\"\\n' > {p}/newcli\nchmod 755 {p}/newcli\n",
                p = prefix_bin.display()
            ),
        );
        let mut t = tool("newcli", "newcli");
        t.install_hint = Some("fake-install".into());

        let env_path = b.env_path();
        assert!(!tools::binary_on_path("newcli", &env_path), "not launchable before");
        match install_tool(&t, &env_path, &b.home) {
            InstallOutcome::Installed { version, via } => {
                assert_eq!(version, "newcli 1.2.3");
                assert!(via.contains(".local/bin/newcli"), "the row names the link: {via}");
            }
            other => panic!("expected Installed, got {other:?}"),
        }
        assert!(tools::binary_on_path("newcli", &env_path), "launchable now, with no restart");
        // Everything written is under $HOME.
        assert!(b.home.join(".local/bin/newcli").symlink_metadata().unwrap().file_type().is_symlink());
        // Re-running is a no-op, never a second install.
        assert!(matches!(install_tool(&t, &env_path, &b.home), InstallOutcome::AlreadyPresent { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn exited_zero_and_installed_nothing_is_failed() {
        // Trusting the exit code would report success; the re-probe reports truth.
        let b = Box_::new("noop");
        let mut t = tool("ghost", "ghost-cli");
        t.install_hint = Some("true".into());
        match install_tool(&t, &b.env_path(), &b.home) {
            InstallOutcome::Failed { error } => {
                assert!(error.contains("still isn't on the session PATH"), "{error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_installer_reports_its_own_error() {
        let b = Box_::new("failing");
        b.write("bad-install", "#!/bin/sh\necho 'npm ERR! ENOSPC no space left' >&2\nexit 28\n");
        let mut t = tool("ghost", "ghost-cli");
        t.install_hint = Some("bad-install".into());
        match install_tool(&t, &b.env_path(), &b.home) {
            InstallOutcome::Failed { error } => assert!(error.contains("ENOSPC"), "{error}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_prerequisite_ends_the_row_with_the_true_reason() {
        // `kimi` needs `uv`; with neither present and uv's installer unavailable,
        // the row must blame uv rather than surfacing `uv: not found`.
        let b = Box_::new("prereq");
        let t = tool("kimi", "kimi");
        assert_eq!(tools::prereqs_for(&t).len(), 1, "kimi declares uv");
        // Point uv's installer at something that can't work, via a PATH with no
        // curl-able network: the fake `curl` fails.
        b.write("curl", "#!/bin/sh\necho 'curl: (6) could not resolve host' >&2\nexit 6\n");
        match install_tool(&t, &b.env_path(), &b.home) {
            InstallOutcome::Failed { error } => {
                assert!(error.contains("uv"), "the row names the prerequisite: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_landing_zone_never_clobbers_a_file_the_user_put_there() {
        let b = Box_::new("clobber");
        let mine = b.home.join(".local").join("bin").join("newcli");
        std::fs::write(&mine, "#!/bin/sh\necho mine\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let prefix_bin = b.home.join(".local").join("share").join("node").join("bin");
        std::fs::create_dir_all(&prefix_bin).unwrap();
        let src = prefix_bin.join("newcli");
        std::fs::write(&src, "#!/bin/sh\necho theirs\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = install_env(&b.env_path(), &b.home);
        let err = land_in_local_bin("newcli", &env, &b.home).unwrap_err();
        assert!(err.contains("isn't ours"), "{err}");
        // …and the user's file is untouched.
        assert_eq!(std::fs::read_to_string(&mine).unwrap(), "#!/bin/sh\necho mine\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_install_timeout_bounds_a_hung_installer() {
        let b = Box_::new("hang");
        let mut t = tool("ghost", "ghost-cli");
        t.install_hint = Some("sleep 30".into());
        std::env::set_var("CCWEB_INSTALL_TIMEOUT_SECS", "1");
        let started = std::time::Instant::now();
        let out = install_tool(&t, &b.env_path(), &b.home);
        std::env::remove_var("CCWEB_INSTALL_TIMEOUT_SECS");
        assert!(started.elapsed() < Duration::from_secs(15), "the timeout must bound the job");
        match out {
            InstallOutcome::Failed { error } => assert!(error.contains("timed out"), "{error}"),
            other => panic!("expected Failed(timed out), got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_plan_names_the_command_and_only_the_missing_prerequisites() {
        let b = Box_::new("plan");
        let env_path = b.env_path();
        let t = tool("codex", "codex");
        let item = plan_tool(&t, &env_path).expect("codex is missing on this fake box");
        assert_eq!(item.tool, "codex");
        assert!(item.command.contains("@openai/codex"), "{}", item.command);
        assert!(!item.size_hint.is_empty());
        // npm isn't on this PATH, so it's listed — with its own command.
        assert_eq!(item.prereqs.len(), 1);
        assert_eq!(item.prereqs[0].key, "npm");
        assert!(!item.prereqs[0].command.is_empty());
        // A tool that IS present has no plan row at all.
        b.write("here-cli", "#!/bin/sh\necho 1.0\n");
        assert!(plan_tool(&tool("here", "here-cli"), &env_path).is_none());
        // A cc_tool_install override short-circuits prerequisite logic entirely.
        let mut custom = tool("codex", "codex");
        custom.install_hint = Some("my-mirror install codex".into());
        let item = plan_tool(&custom, &env_path).unwrap();
        assert_eq!(item.command, "my-mirror install codex");
        assert!(item.prereqs.is_empty(), "the self-hoster said what they wanted");
    }

    #[cfg(unix)]
    #[test]
    fn no_builtin_install_command_uses_sudo() {
        // Part B5: `sudo` is never *added*. A user-declared cc_tool_install is
        // run as written, but nothing we ship escalates.
        for a in tools::ASSISTANTS {
            for c in [a.install_macos, a.install_linux, a.install_windows] {
                assert!(!c.contains("sudo"), "{} install command escalates: {c}", a.prefix);
            }
        }
        for p in tools::PREREQS {
            for c in [p.install_macos, p.install_linux, p.install_windows] {
                assert!(!c.contains("sudo"), "prereq {} escalates: {c}", p.key);
            }
        }
    }
}
