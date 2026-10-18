// The agent replaces itself with the latest release (proposal 0091).
//
// Proposal 0049's Update button kept the coding-assistant CLIs current, but the
// agent driving them could only be updated by hand on the machine
// (`cc-screen-rust update`). This module is the first step of that same job when
// the client asks for it: fetch the release installer, decide from ITS version
// whether there is anything newer, install it next to the running binary, prove
// the installed binary reports the new version, then exit so the service manager
// starts the new one — which reads a marker file and finishes the job.
//
// Three rules carry the design:
//
// - **Only where it can work.** A POSIX agent running from the installer's own
//   path, under launchd or systemd. Anywhere else (a binary built in a source
//   tree, a foreground run, Windows) it says so instead of trying: updating a
//   file nobody runs, or exiting with nothing to restart us, are both worse than
//   "run `cc-screen-rust update` on it". `support` is that whole decision, and
//   the uplink advertises `agent-update` only when it passes.
// - **Versions decide, not exit codes** — the 0049 discipline. Nothing is
//   installed unless the release is strictly newer than what runs (a dev build
//   ahead of the release is never "updated" backwards), and the restart happens
//   only after the installed binary's own `--version` reports the release.
// - **No command travels on the wire.** The installer URL is the compiled-in
//   `RELEASE_BASE_URL`, the same one the README's one-liner uses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cc_screen_protocol::{UpdateJob, UpdateReq, UpdateToolStatus};
use serde::{Deserialize, Serialize};

use crate::assistants::{run_shell, run_shell_ex, tail_error};
use crate::engine::AppState;

/// The agent row's `tool` / `label` in the job snapshot.
pub const ROW_TOOL: &str = "cc-screen";
pub const ROW_LABEL: &str = "cc-screen agent";
/// The file the old agent leaves for the new one, in the config dir.
const MARKER: &str = "agent-update.json";
/// The downloaded installer, kept only for the length of one attempt.
const INSTALLER: &str = "agent-installer.sh";
/// What the agent exits with to be restarted. Non-zero on purpose: launchd's
/// `KeepAlive` and systemd's `Restart=always` restart on any exit, a hand-written
/// `Restart=on-failure` unit only on a failing one. 75 is `EX_TEMPFAIL`.
pub const RESTART_EXIT_CODE: i32 = 75;
/// A marker older than this is a leftover (the new agent never came up, the
/// machine was off for a week) and is discarded rather than reported.
const MARKER_MAX_AGE_SECS: u64 = 30 * 60;
/// Long enough for a polling client to see "restarting" before the machine
/// drops off the hub.
const RESTART_GRACE: Duration = Duration::from_secs(3);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// The version this process was built as.
pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The installer's own name for the binary, per platform.
fn bin_name() -> &'static str {
    if cfg!(windows) { "cc-screen-rust.exe" } else { "cc-screen-rust" }
}

/// Whether this agent can replace itself, and where the binary lives if so. The
/// inputs are injected so the decision is testable without being a service.
pub fn support_with(
    exe: Option<&Path>,
    home: &Path,
    env: impl Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    if cfg!(windows) {
        return Err("a Windows agent can't update itself yet — re-run its install command on the machine".into());
    }
    let installed = home.join(".local").join("bin").join(bin_name());
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let Some(exe) = exe else {
        return Err("can't tell where this agent runs from".into());
    };
    if canon(exe) != canon(&installed) {
        return Err(format!(
            "this agent runs from {}, not the installer's ~/.local/bin — update it where it was built",
            exe.display()
        ));
    }
    let launchd = env("XPC_SERVICE_NAME").is_some_and(|v| v == crate::service::LAUNCHD_LABEL);
    let systemd = env("INVOCATION_ID").is_some_and(|v| !v.is_empty());
    if !launchd && !systemd {
        return Err("this agent isn't running as a service, so nothing would start it again — run `cc-screen-rust update` on it".into());
    }
    Ok(installed.parent().map(Path::to_path_buf).unwrap_or_default())
}

/// [`support_with`] for this process.
pub fn support(home: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().ok();
    support_with(exe.as_deref(), home, |k| std::env::var(k).ok())
}

/// `MAJOR.MINOR.PATCH`, ignoring any pre-release/build suffix. `None` for
/// anything else — the caller then refuses rather than guessing.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

/// The version a cargo-dist shell installer installs: its `APP_VERSION="x"` line.
fn installer_version(script: &str) -> Option<String> {
    script.lines().find_map(|l| {
        let v = l.trim().strip_prefix("APP_VERSION=\"")?.strip_suffix('"')?;
        parse_version(v).map(|_| v.to_string())
    })
}

/// What `cc-screen-rust --version` printed, as a bare version.
fn reported_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("cc-screen-rust ").map(|v| v.trim().to_string()))
        .filter(|v| parse_version(v).is_some())
}

/// The handover from the old agent to the new one.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Marker {
    /// The job as the old agent left it (agent row `restarting`).
    job: UpdateJob,
    /// The rest of the request, which the new agent carries out.
    req: UpdateReq,
    from: String,
    to: String,
    written_at: u64,
}

/// What the first step of the job decided.
#[derive(Debug, PartialEq)]
pub enum AgentStep {
    /// Carry on with the assistants in this process.
    Continue,
    /// The new release is installed and the marker written: exit, so the
    /// service manager starts it.
    Restart,
}

fn set_row(app: &AppState, f: impl FnOnce(&mut UpdateToolStatus)) {
    app.with_update_job(|j| {
        if let Some(row) = j.agent.as_mut() {
            f(row);
        }
    });
}

/// The pending agent row a job starts with.
pub fn pending_row() -> UpdateToolStatus {
    UpdateToolStatus {
        tool: ROW_TOOL.into(),
        label: ROW_LABEL.into(),
        state: "pending".into(),
        from: Some(running_version().to_string()),
        ..Default::default()
    }
}

/// Step one of an update job that asked for the agent too. Every outcome except
/// a proven install ends in `Continue`, with the row saying why.
pub fn run_agent_step(app: &AppState, req: &UpdateReq) -> AgentStep {
    let base = format!("{}/cc-screen-rust-installer.sh", cc_screen_protocol::RELEASE_BASE_URL);
    run_agent_step_from(app, req, &base, "=https", support(&app.inner.home))
}

/// [`run_agent_step`] with the installer URL (and the protocols curl may use to
/// fetch it) and the support verdict injected, so a test can serve its own
/// installer from a file and skip the service-manager check.
fn run_agent_step_from(
    app: &AppState,
    req: &UpdateReq,
    installer_url: &str,
    protos: &str,
    support: Result<PathBuf, String>,
) -> AgentStep {
    let current = running_version().to_string();
    set_row(app, |r| r.state = "updating".into());
    let dir = match support {
        Ok(d) => d,
        Err(why) => {
            set_row(app, |r| {
                r.state = "skipped".into();
                r.message = Some(why);
            });
            return AgentStep::Continue;
        }
    };
    let fail = |msg: String| {
        set_row(app, |r| {
            r.state = "failed".into();
            r.to = None;
            r.message = Some(msg);
        });
        AgentStep::Continue
    };
    let env_path = app.inner.env_path.clone();
    let script = app.inner.config_dir.join(INSTALLER);
    let _ = std::fs::remove_file(&script);

    // 1. Fetch the installer and read what it would install.
    let fetch = format!(
        "curl --proto {} --tlsv1.2 -fsSL {} -o {}",
        crate::tools::shell_quote(protos),
        crate::tools::shell_quote(installer_url),
        crate::tools::shell_quote(&script.to_string_lossy()),
    );
    let run = run_shell(&fetch, &env_path, FETCH_TIMEOUT, &[]);
    if !run.ok {
        return fail(tail_error(&run.output, run.error.as_deref().unwrap_or("could not download the installer")));
    }
    let text = std::fs::read_to_string(&script).unwrap_or_default();
    let Some(latest) = installer_version(&text) else {
        let _ = std::fs::remove_file(&script);
        return fail("could not read the latest release's version from its installer".into());
    };
    let (Some(have), Some(want)) = (parse_version(&current), parse_version(&latest)) else {
        let _ = std::fs::remove_file(&script);
        return fail(format!("can't compare versions {current} and {latest}"));
    };
    if want <= have {
        let _ = std::fs::remove_file(&script);
        set_row(app, |r| {
            r.state = "current".into();
            r.to = Some(current.clone());
            if want < have {
                r.message = Some(format!("newer than the latest release ({latest})"));
            }
        });
        return AgentStep::Continue;
    }

    // 2. Install it next to the running binary. The installer replaces the file
    //    with a rename, so the running process is untouched; it must not edit the
    //    user's shell profile (the path is already on PATH — we run from it).
    let install = format!("sh {}", crate::tools::shell_quote(&script.to_string_lossy()));
    let extra = [
        ("CC_SCREEN_RUST_INSTALL_DIR", dir.to_string_lossy().into_owned()),
        ("CC_SCREEN_RUST_NO_MODIFY_PATH", "1".to_string()),
        ("CC_SCREEN_RUST_PRINT_QUIET", "1".to_string()),
    ];
    let run = run_shell_ex(&install, &env_path, INSTALL_TIMEOUT, &extra, &[]);
    let _ = std::fs::remove_file(&script);
    if !run.ok {
        return fail(tail_error(&run.output, run.error.as_deref().unwrap_or("the installer failed")));
    }

    // 3. The installed binary's own answer is the verdict.
    let bin = dir.join(bin_name());
    let probe = format!("{} --version", crate::tools::shell_quote(&bin.to_string_lossy()));
    let run = run_shell(&probe, &env_path, VERSION_TIMEOUT, &[]);
    let got = reported_version(&run.output);
    if got.as_deref() != Some(latest.as_str()) {
        return fail(format!(
            "installed {latest}, but {} reports {}",
            bin.display(),
            got.unwrap_or_else(|| "no version".into())
        ));
    }

    // 4. Hand over. The row says `restarting` in the marker AND in the live
    //    snapshot, so a client polling right now sees it before we go.
    set_row(app, |r| {
        r.state = "restarting".into();
        r.to = Some(latest.clone());
        r.message = None;
    });
    let rest = UpdateReq { update_agent: false, ..req.clone() };
    let marker = Marker { job: app.update_job(), req: rest, from: current, to: latest, written_at: now() };
    if let Err(e) = write_marker(&app.inner.config_dir, &marker) {
        // Without the marker the new agent couldn't finish the job — and nothing
        // is lost by staying on the old binary until the next restart.
        return fail(format!("installed, but could not record the handover: {e}"));
    }
    AgentStep::Restart
}

fn now() -> u64 {
    crate::engine::now_secs()
}

fn write_marker(config_dir: &Path, m: &Marker) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    let tmp = config_dir.join(format!("{MARKER}.tmp"));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, config_dir.join(MARKER)).map_err(|e| e.to_string())
}

/// Read and REMOVE the marker, so a crash during the follow-up can never loop.
fn take_marker(config_dir: &Path) -> Option<Marker> {
    let path = config_dir.join(MARKER);
    let body = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    let m: Marker = serde_json::from_slice(&body?).ok()?;
    (now().saturating_sub(m.written_at) <= MARKER_MAX_AGE_SECS).then_some(m)
}

/// Let the in-flight job's last snapshot reach a poller, then exit for the
/// service manager to restart us on the new binary.
pub fn exit_into_new_binary() -> ! {
    std::thread::sleep(RESTART_GRACE);
    tracing::info!("cc-screen-rust: restarting into the updated agent");
    std::process::exit(RESTART_EXIT_CODE);
}

/// At startup, after restore: finish a job the previous agent handed over. The
/// agent row is decided by the version THIS process runs — the only proof the
/// restart landed on the new binary — and the assistants step runs as asked.
pub fn resume_after_restart(app: &AppState) {
    let Some(m) = take_marker(&app.inner.config_dir) else { return };
    let mut job = m.job;
    let running = running_version();
    let landed = running == m.to;
    if let Some(row) = job.agent.as_mut() {
        if landed {
            row.state = "updated".into();
            row.from = Some(m.from.clone());
            row.to = Some(m.to.clone());
            row.message = None;
        } else {
            row.state = "failed".into();
            row.message = Some(format!("restarted, but this agent is still {running}"));
        }
    }
    tracing::info!("cc-screen-rust: agent update {} → {}: {}", m.from, m.to, if landed { "done" } else { "did not land" });
    if landed {
        // The new binary may carry a newer clipboard shim (0007/0077).
        if let Err(e) = crate::service::install_shim() {
            tracing::warn!("cc-screen-rust: could not refresh the clipboard shim: {e}");
        }
    }
    crate::assistants::continue_job(app, job, &m.req);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_strictly() {
        assert_eq!(parse_version("0.5.10"), Some((0, 5, 10)));
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("v1.2.3"), None);
        // Numeric, not lexical: 0.5.10 is newer than 0.5.9.
        assert!(parse_version("0.5.10") > parse_version("0.5.9"));
    }

    #[test]
    fn installer_and_binary_versions_are_read_from_their_own_output() {
        let script = "#!/bin/sh\nAPP_NAME=\"cc-screen-rust\"\nAPP_VERSION=\"0.5.10\"\n";
        assert_eq!(installer_version(script).as_deref(), Some("0.5.10"));
        assert_eq!(installer_version("APP_VERSION=\"garbage\"\n"), None);
        assert_eq!(reported_version("cc-screen-rust 0.5.10\n").as_deref(), Some("0.5.10"));
        assert_eq!(reported_version("listening on 127.0.0.1\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn support_needs_the_installer_path_and_a_service_manager() {
        let home = std::env::temp_dir().join(format!("ccr-su-{}", std::process::id()));
        let bin = home.join(".local").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("cc-screen-rust");
        std::fs::write(&exe, b"x").unwrap();
        let none = |_: &str| None;
        let launchd = |k: &str| (k == "XPC_SERVICE_NAME").then(|| crate::service::LAUNCHD_LABEL.to_string());
        let systemd = |k: &str| (k == "INVOCATION_ID").then(|| "abc".to_string());
        let other_launchd = |k: &str| (k == "XPC_SERVICE_NAME").then(|| "com.apple.Terminal".to_string());

        assert_eq!(support_with(Some(&exe), &home, launchd).unwrap(), bin);
        assert_eq!(support_with(Some(&exe), &home, systemd).unwrap(), bin);
        let fg = support_with(Some(&exe), &home, none).unwrap_err();
        assert!(fg.contains("isn't running as a service"), "{fg}");
        // A Terminal tab also sets XPC_SERVICE_NAME — to its own label.
        assert!(support_with(Some(&exe), &home, other_launchd).is_err());
        let src = home.join("src").join("target").join("release").join("cc-screen-rust");
        let built = support_with(Some(&src), &home, systemd).unwrap_err();
        assert!(built.contains("update it where it was built"), "{built}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A scratch agent whose "release" is a local installer script that drops a
    /// fake `cc-screen-rust` reporting `version` into `bin`.
    #[cfg(unix)]
    struct Rig {
        root: PathBuf,
        bin: PathBuf,
        app: AppState,
    }

    #[cfg(unix)]
    impl Rig {
        fn new(tag: &str) -> Rig {
            let root = std::env::temp_dir().join(format!("ccr-su-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let bin = root.join("home").join(".local").join("bin");
            let cfg = root.join("cfg");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::create_dir_all(&cfg).unwrap();
            let app = AppState::new(
                Vec::new(),
                std::env::var("PATH").unwrap_or_default(),
                String::new(),
                cfg.clone(),
                root.join("home"),
                "test-agent".into(),
                crate::auth::Auth::load(&cfg, None, None),
                cc_screen_auth::OriginPolicy::default(),
            );
            Rig { root, bin, app }
        }

        /// Write an installer that installs a binary reporting `reports`, while
        /// claiming `APP_VERSION=version`. Returns its file:// URL.
        fn installer(&self, version: &str, reports: &str) -> String {
            let path = self.root.join("installer.sh");
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nAPP_VERSION=\"{version}\"\n\
                     printf '#!/bin/sh\\necho \"cc-screen-rust {reports}\"\\n' > \"$CC_SCREEN_RUST_INSTALL_DIR/cc-screen-rust\"\n\
                     chmod 755 \"$CC_SCREEN_RUST_INSTALL_DIR/cc-screen-rust\"\n"
                ),
            )
            .unwrap();
            format!("file://{}", path.display())
        }

        fn job(&self) {
            let mut job = UpdateJob { id: "j1".into(), phase: "updating".into(), ..Default::default() };
            job.agent = Some(pending_row());
            self.app.begin_update_job(job).unwrap();
        }

        fn row(&self) -> UpdateToolStatus {
            self.app.update_job().agent.unwrap()
        }
    }

    #[cfg(unix)]
    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn bump(v: &str) -> String {
        let (a, b, c) = parse_version(v).unwrap();
        format!("{a}.{b}.{}", c + 1)
    }

    // The happy path, end to end across the "restart": install, prove the new
    // binary, hand over through the marker, and let the next process finish.
    #[cfg(unix)]
    #[test]
    fn a_newer_release_is_installed_proven_and_handed_over() {
        let rig = Rig::new("newer");
        let next = bump(running_version());
        let url = rig.installer(&next, &next);
        rig.job();
        let req = UpdateReq { tools: vec!["claude".into()], update_agent: true, ..Default::default() };
        assert_eq!(run_agent_step_from(&rig.app, &req, &url, "=file", Ok(rig.bin.clone())), AgentStep::Restart);
        let row = rig.row();
        assert_eq!(row.state, "restarting");
        assert_eq!(row.to.as_deref(), Some(next.as_str()));
        assert!(!rig.app.inner.config_dir.join(INSTALLER).exists(), "the downloaded installer is removed");

        // The marker carries the REST of the request — never update_agent again,
        // or the new agent would loop.
        let m = take_marker(&rig.app.inner.config_dir).expect("marker written");
        assert_eq!(m.to, next);
        assert!(!m.req.update_agent);
        assert_eq!(m.req.tools, vec!["claude".to_string()]);
        assert!(take_marker(&rig.app.inner.config_dir).is_none(), "taken exactly once");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_installed_when_the_release_is_not_newer() {
        let rig = Rig::new("same");
        let url = rig.installer(running_version(), running_version());
        rig.job();
        let step = run_agent_step_from(&rig.app, &UpdateReq::default(), &url, "=file", Ok(rig.bin.clone()));
        assert_eq!(step, AgentStep::Continue);
        assert_eq!(rig.row().state, "current");
        assert!(!rig.bin.join("cc-screen-rust").exists(), "the installer never ran");
        assert!(take_marker(&rig.app.inner.config_dir).is_none());
    }

    // A dev build ahead of the release must never be "updated" backwards.
    #[cfg(unix)]
    #[test]
    fn an_agent_ahead_of_the_release_is_never_downgraded() {
        let rig = Rig::new("ahead");
        let url = rig.installer("0.0.1", "0.0.1");
        rig.job();
        let step = run_agent_step_from(&rig.app, &UpdateReq::default(), &url, "=file", Ok(rig.bin.clone()));
        assert_eq!(step, AgentStep::Continue);
        let row = rig.row();
        assert_eq!(row.state, "current");
        assert!(row.message.unwrap_or_default().contains("newer than the latest release"));
        assert!(!rig.bin.join("cc-screen-rust").exists());
    }

    // The installer "succeeds" but the binary it left isn't the release: no
    // restart, an honest failure, and the job goes on with the assistants.
    #[cfg(unix)]
    #[test]
    fn an_install_the_binary_does_not_confirm_is_a_failure_without_restart() {
        let rig = Rig::new("lie");
        let next = bump(running_version());
        let url = rig.installer(&next, running_version());
        rig.job();
        let step = run_agent_step_from(&rig.app, &UpdateReq::default(), &url, "=file", Ok(rig.bin.clone()));
        assert_eq!(step, AgentStep::Continue);
        let row = rig.row();
        assert_eq!(row.state, "failed");
        assert!(row.message.unwrap_or_default().contains("reports"));
        assert!(take_marker(&rig.app.inner.config_dir).is_none(), "no handover without proof");
    }

    #[cfg(unix)]
    #[test]
    fn an_unsupported_agent_says_why_and_carries_on() {
        let rig = Rig::new("unsupported");
        rig.job();
        let step = run_agent_step_from(&rig.app, &UpdateReq::default(), "file:///nonexistent", "=file", Err("runs from a source tree".into()));
        assert_eq!(step, AgentStep::Continue);
        let row = rig.row();
        assert_eq!(row.state, "skipped");
        assert_eq!(row.message.as_deref(), Some("runs from a source tree"));
    }

    // The new process's half: the row is decided by the version it runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_new_agent_finishes_the_job_by_its_own_version() {
        for (to, want) in [(running_version().to_string(), "updated"), ("99.0.0".to_string(), "failed")] {
            let rig = Rig::new(&format!("resume-{want}"));
            let mut job = UpdateJob { id: "j9".into(), phase: "updating".into(), ..Default::default() };
            let mut row = pending_row();
            row.state = "restarting".into();
            job.agent = Some(row);
            let m = Marker {
                job,
                req: UpdateReq { tools: vec!["nothing-here".into()], ..Default::default() },
                from: "0.0.1".into(),
                to: to.clone(),
                written_at: now(),
            };
            write_marker(&rig.app.inner.config_dir, &m).unwrap();
            resume_after_restart(&rig.app);
            let mut j = rig.app.update_job();
            for _ in 0..200 {
                if j.phase == "done" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                j = rig.app.update_job();
            }
            assert_eq!(j.id, "j9", "the same job continues, so a client keeps watching it");
            assert_eq!(j.phase, "done");
            assert_eq!(j.agent.unwrap().state, want);
            assert!(!rig.app.inner.config_dir.join(MARKER).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_marker_is_discarded() {
        let rig = Rig::new("stale");
        let m = Marker {
            job: UpdateJob::idle(),
            req: UpdateReq::default(),
            from: "0.0.1".into(),
            to: "0.0.2".into(),
            written_at: now() - MARKER_MAX_AGE_SECS - 10,
        };
        write_marker(&rig.app.inner.config_dir, &m).unwrap();
        assert!(take_marker(&rig.app.inner.config_dir).is_none());
        assert!(!rig.app.inner.config_dir.join(MARKER).exists(), "and removed");
    }
}
