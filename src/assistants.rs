// The assistant-CLI update runner (proposal 0049). cc-screen *drives* external
// CLIs (claude, codex, gemini, kimi), so keeping them current is a first-class
// operation — and the four update in three different ways: `claude update` and
// `codex update` are self-update subcommands, while `gemini` and `kimi` have none
// and are updated through the package manager that installed them.
//
// This module owns exactly one job: run a tool's update and return a TRUTHFUL
// outcome. It generalises `doctor`'s probe-decides-the-verdict discipline
// (`doctor.rs`: run the installer, then re-probe — the probe is the verdict) from
// "is it there?" to "did its version change?". Every one of these updaters exits
// 0 when there was nothing to do, and some exit 0 having done nothing useful, so
// the exit code is NOT the signal: the `--version` output, compared before and
// after as an opaque string, is.
//
// Nothing here touches sessions. Restarting the sessions that use an updated CLI
// is `engine::restart_sessions`; the two are sequenced by the job in `engine.rs`.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::tools::{self, Tool};

/// Per-command wall clock before we kill the child and move to the next
/// candidate. An `npm install -g` on a cold cache is genuinely slow; a hung one
/// must not wedge the job. Override with `CCWEB_UPDATE_TIMEOUT_SECS`.
const DEFAULT_UPDATE_TIMEOUT_SECS: u64 = 300;
/// A `--version` probe answers immediately or it's broken.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
/// Cap on a captured version line — these are one short line in practice.
const MAX_VERSION_LEN: usize = 200;
/// Cap on a captured error tail, so a row can show `npm ERR! EACCES …` without
/// shipping a megabyte of build log to a phone.
const MAX_ERROR_LEN: usize = 500;

/// The outcome of updating ONE tool. `Updated` is claimed only when the version
/// string actually moved.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateOutcome {
    /// The version string changed — the only thing that counts as an update.
    Updated { from: String, to: String },
    /// An update command ran and succeeded, but the version is unchanged.
    Current { version: String },
    /// Every candidate failed (non-zero, timed out, or unrunnable).
    Failed { version: String, error: String },
    /// Not installed, or no known update command — never an error.
    Skipped { reason: String },
}

fn update_timeout() -> Duration {
    let secs = std::env::var("CCWEB_UPDATE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_UPDATE_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// What running one command produced: its exit status (`None` = timed out or
/// never started) and the combined stdout+stderr.
pub(crate) struct Run {
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Run `line` through the session's launch shell with `PATH = env_path`,
/// capturing output, with a hard timeout. **stdin is /dev/null** — this runs
/// unattended inside the agent, so a command that decides to prompt must fail
/// rather than block forever (the consent for this already happened in the UI).
///
/// `extra_env` are additional variables for this one command (proposal 0050's
/// install environment: `npm_config_prefix`, `UV_TOOL_BIN_DIR`, …) — set on the
/// child only, so the user's own config is never rewritten.
pub(crate) fn run_shell(
    line: &str,
    env_path: &str,
    timeout: Duration,
    extra_env: &[(&str, String)],
) -> Run {
    run_shell_ex(line, env_path, timeout, extra_env, &[])
}

pub(crate) fn run_shell_ex(
    line: &str,
    env_path: &str,
    timeout: Duration,
    extra_env: &[(&str, String)],
    strip_env: &[&str],
) -> Run {
    let (program, pre_args) = tools::launch_shell();
    let mut cmd = Command::new(program);
    for a in pre_args {
        cmd.arg(a);
    }
    cmd.arg(line)
        .env("PATH", env_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    for k in strip_env {
        cmd.env_remove(k);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Run { ok: false, output: String::new(), error: Some(format!("could not run `{line}`: {e}")) }
        }
    };
    // Drain both pipes on their own threads: a child that fills a pipe buffer
    // would otherwise block forever while we sat in try_wait.
    let mut readers: Vec<std::thread::JoinHandle<String>> = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        readers.push(std::thread::spawn(move || read_all(&mut out)));
    }
    if let Some(mut err) = child.stderr.take() {
        readers.push(std::thread::spawn(move || read_all(&mut err)));
    }

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break None,
        }
    };
    if timed_out {
        // Deliberately DON'T join the readers: killing the child doesn't reap a
        // grandchild it spawned (an `npm` wrapper's real worker), which keeps the
        // pipes open — joining here would wait out exactly the hang the timeout
        // exists to bound. The threads end on their own when the pipes close, and
        // "timed out" is the whole message anyway.
        return Run {
            ok: false,
            output: String::new(),
            error: Some(format!("timed out after {}s", timeout.as_secs())),
        };
    }
    let output: String = readers.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>().join("");
    let error = match status {
        Some(s) if s.success() => None,
        Some(s) => Some(format!("`{line}` exited with {s}")),
        None => Some(format!("`{line}` did not run")),
    };
    Run { ok: error.is_none(), output, error }
}

/// Read a child pipe to EOF, lossily — updater output is human text, and a
/// stray non-UTF-8 byte must never lose us the error message.
fn read_all<R: Read>(r: &mut R) -> String {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The tool's version as an opaque string: the first non-empty line of its
/// `--version` output, trimmed and capped. Empty when the probe fails — still
/// comparable, and the caller falls back to the exit code when both ends are
/// empty. We deliberately never parse semver: "did it change?" is the only
/// question, and string inequality answers it without a parser a vendor's format
/// change can break.
pub fn probe_version(t: &Tool, env_path: &str) -> String {
    let Some(bin) = tools::probe_binary(t) else { return String::new() };
    let line = format!("{bin} {}", tools::version_arg(t));
    let run = run_shell(&line, env_path, VERSION_TIMEOUT, &[]);
    if !run.ok && run.output.trim().is_empty() {
        return String::new();
    }
    first_line(&run.output)
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(MAX_VERSION_LEN).collect())
        .unwrap_or_default()
}

/// The last few meaningful lines of a failed command's output, for the row's
/// message. Bounded so a build log can't flood the UI.
pub(crate) fn tail_error(output: &str, fallback: &str) -> String {
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = lines.iter().rev().take(3).rev().copied().collect::<Vec<_>>().join(" · ");
    let msg = if tail.is_empty() { fallback.to_string() } else { format!("{fallback}: {tail}") };
    msg.chars().take(MAX_ERROR_LEN).collect()
}

/// Update one tool and report what actually happened.
///
/// 1. **Presence first** — a missing CLI is `Skipped`, never installed here.
///    Installing is `doctor --install`: a different intent with different consent.
/// 2. **Version before**, via `probe_version`.
/// 3. **The ordered candidates** (`tools::update_commands`) until one moves the
///    version — the self-updater first (the CLI knows how it was installed), the
///    package manager after (because a self-updater on a package-manager-installed
///    copy may legitimately refuse).
/// 4. **Version after decides.** Changed → `Updated`. Unchanged but the command
///    succeeded → `Current`. Unchanged and it failed → try the next candidate; if
///    none remain → `Failed` with the command's own error text.
pub fn update_tool(t: &Tool, env_path: &str, home: &std::path::Path) -> UpdateOutcome {
    if let Some(bin) = tools::missing_binary(t, env_path) {
        return UpdateOutcome::Skipped { reason: format!("{bin} is not installed") };
    }
    let cmds = tools::update_commands(t);
    if cmds.is_empty() {
        return UpdateOutcome::Skipped {
            reason: "no known update command (declare one with cc_tool_update)".into(),
        };
    }
    let before = probe_version(t, env_path);
    let timeout = update_timeout();
    let mut last_error: Option<String> = None;
    let wrap = crate::vendor_guard::wraps_update(t);

    for cmd in &cmds {
        let run = if wrap {
            match crate::vendor_guard::Guard::begin(home) {
                Ok(g) => {
                    let run = g.run(cmd, env_path, timeout, &[]);
                    if let Err(e) = g.finish() {
                        return UpdateOutcome::Failed {
                            version: after_or(&probe_version(t, env_path), &before),
                            error: e,
                        };
                    }
                    run
                }
                Err(e) => {
                    return UpdateOutcome::Failed {
                        version: after_or(&before, &before),
                        error: e,
                    };
                }
            }
        } else {
            run_shell(cmd, env_path, timeout, &[])
        };
        let after = probe_version(t, env_path);
        if !after.is_empty() && after != before {
            return UpdateOutcome::Updated { from: before, to: after };
        }
        if run.ok {
            // It ran cleanly and nothing moved — genuinely up to date. Stop:
            // trying the package manager after a clean self-update would only
            // reinstall the same version.
            return UpdateOutcome::Current { version: after_or(&after, &before) };
        }
        last_error = Some(tail_error(&run.output, run.error.as_deref().unwrap_or("update failed")));
    }

    // Every candidate failed and the version never moved.
    let version = probe_version(t, env_path);
    UpdateOutcome::Failed {
        version: after_or(&version, &before),
        error: last_error.unwrap_or_else(|| "update failed".into()),
    }
}

/// Prefer the freshly-probed version, falling back to the one we read before the
/// attempt (an updater that broke the probe shouldn't blank the row).
fn after_or(after: &str, before: &str) -> String {
    if after.is_empty() { before.to_string() } else { after.to_string() }
}

// ── The two-phase job (proposal 0049 Part C) ─────────────────────────────────
// An update can take minutes, which is longer than a hub relay's reply timeout
// and longer than a phone keeps its screen awake — so this is a job the agent
// owns and clients read, not one long request. Phase 1 updates the CLIs; phase 2
// restarts the sessions that use them. Update-first is deliberate: if the update
// fails, the sessions were never touched and the machine is exactly as it was.

use cc_screen_protocol::{SessionRestartStatus, UpdateJob, UpdateReq, UpdateToolStatus};

use crate::engine::AppState;

/// Monotonic suffix so two jobs started in the same second still get distinct
/// ids (the client uses the id to notice a *new* run, not to order them).
static JOB_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The tools this job will touch: every registered tool that names a real binary
/// (`probe_binary`), narrowed to `wanted` (by prefix or cmd) when that isn't
/// empty. The bare `shell` tool's template head is a shell expansion, so it
/// drops out structurally — it isn't an assistant, has nothing to update and no
/// resume. A *custom* `tools.conf` tool is included: with a `cc_tool_update` it
/// updates like any other, and without one it reports `skipped: no known update
/// command`, which is the honest answer rather than silence.
fn selected_tools(app: &AppState, wanted: &[String]) -> Vec<Tool> {
    app.inner
        .tools
        .iter()
        .filter(|t| tools::probe_binary(t).is_some())
        .filter(|t| wanted.is_empty() || wanted.iter().any(|w| w == &t.prefix || w == &t.cmd))
        .cloned()
        .collect()
}

/// Start the update-then-restart job. Returns the fresh snapshot, or `Err` with
/// the running job's snapshot when one is already live (the caller answers 409
/// and the client watches that one instead of racing a second).
pub fn start_job(app: &AppState, req: &UpdateReq) -> Result<UpdateJob, UpdateJob> {
    let selected = selected_tools(app, &req.tools);
    let seq = JOB_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let job = UpdateJob {
        id: format!("{}-{seq}", crate::engine::now_secs()),
        phase: "updating".into(),
        started_at: crate::engine::now_secs() as i64,
        finished_at: None,
        tools: selected
            .iter()
            .map(|t| UpdateToolStatus {
                tool: t.prefix.clone(),
                label: tools::assistant_for(t).map(|a| a.label.to_string()).unwrap_or_else(|| t.prefix.clone()),
                state: "pending".into(),
                ..Default::default()
            })
            .collect(),
        sessions: Vec::new(),
        error: None,
    };
    let started = app.begin_update_job(job)?;

    let app = app.clone();
    let restart_mode = req.restart.clone();
    let install_missing = req.install_missing;
    // A dedicated thread, like the reaper: these are blocking `Command` calls
    // with no business on the async runtime.
    std::thread::spawn(move || run_job(&app, selected, &restart_mode, install_missing));
    Ok(started)
}

/// The worker: phase 1 (update each CLI, one at a time), then phase 2 (restart
/// the affected sessions), writing every transition into the shared snapshot.
fn run_job(app: &AppState, selected: Vec<Tool>, restart_mode: &str, install_missing: bool) {
    let env_path = app.inner.env_path.clone();
    let home = app.inner.home.clone();
    // Serialised on purpose: `codex` and `gemini` are both global npm installs,
    // and concurrent `npm install -g` runs contend on the same prefix — exactly
    // the half-written state this feature exists to avoid. Installing adds a
    // second reason (proposal 0050 C2): two assistants can share a prerequisite,
    // and the second must see the Node the first one installed.
    let mut updated: Vec<String> = Vec::new();
    let mut installed: Vec<String> = Vec::new();
    let mut not_updated: Vec<(String, String)> = Vec::new(); // (prefix, why)
    for t in &selected {
        // The install branch (proposal 0050): a CLI that isn't on the machine at
        // all can't be *updated*, and until now the row simply said so and
        // stopped. `phase` stays `updating` — install and update are the same act
        // on the same row (preparing the CLIs), and a third phase would be a
        // wrong picture on a client that predates it.
        if install_missing && crate::tools::missing_binary(t, &env_path).is_some() {
            set_tool_state(app, &t.prefix, |row| row.state = "installing".into());
            match crate::provision::install_tool(t, &env_path, &home) {
                crate::provision::InstallOutcome::Installed { version, via } => {
                    installed.push(t.prefix.clone());
                    set_tool_state(app, &t.prefix, move |row| {
                        row.state = "installed".into();
                        row.to = Some(version).filter(|s| !s.is_empty());
                        row.message = Some(format!("via {via}"));
                    });
                }
                // Won the race with someone else's install, or a stale probe.
                crate::provision::InstallOutcome::AlreadyPresent { version } => {
                    not_updated.push((t.prefix.clone(), "already current".into()));
                    set_tool_state(app, &t.prefix, move |row| {
                        row.state = "current".into();
                        row.to = Some(version).filter(|s| !s.is_empty());
                    });
                }
                crate::provision::InstallOutcome::Failed { error } => {
                    not_updated.push((t.prefix.clone(), format!("install failed: {error}")));
                    set_tool_state(app, &t.prefix, move |row| {
                        row.state = "failed".into();
                        row.message = Some(error);
                    });
                }
                crate::provision::InstallOutcome::Unsupported { reason } => {
                    not_updated.push((t.prefix.clone(), reason.clone()));
                    set_tool_state(app, &t.prefix, move |row| {
                        row.state = "skipped".into();
                        row.message = Some(reason);
                    });
                }
            }
            continue;
        }
        set_tool_state(app, &t.prefix, |row| row.state = "updating".into());
        let outcome = update_tool(t, &env_path, &home);
        match &outcome {
            UpdateOutcome::Updated { from, to } => {
                updated.push(t.prefix.clone());
                let (from, to) = (from.clone(), to.clone());
                set_tool_state(app, &t.prefix, move |row| {
                    row.state = "updated".into();
                    row.from = Some(from);
                    row.to = Some(to);
                });
            }
            UpdateOutcome::Current { version } => {
                not_updated.push((t.prefix.clone(), "already current".into()));
                let v = version.clone();
                set_tool_state(app, &t.prefix, move |row| {
                    row.state = "current".into();
                    row.to = Some(v);
                });
            }
            UpdateOutcome::Failed { version, error } => {
                not_updated.push((t.prefix.clone(), format!("update failed: {error}")));
                let (v, e) = (version.clone(), error.clone());
                set_tool_state(app, &t.prefix, move |row| {
                    row.state = "failed".into();
                    row.to = Some(v).filter(|s| !s.is_empty());
                    row.message = Some(e);
                });
            }
            UpdateOutcome::Skipped { reason } => {
                not_updated.push((t.prefix.clone(), reason.clone()));
                let r = reason.clone();
                set_tool_state(app, &t.prefix, move |row| {
                    row.state = "skipped".into();
                    row.message = Some(r);
                });
            }
        }
    }

    // Phase 2. `updated` (the default) restarts only sessions whose CLI actually
    // changed — a codex session isn't churned because claude moved. `all` is the
    // escape hatch; `none` updates only.
    let restart_prefixes: Vec<String> = match restart_mode {
        "none" => Vec::new(),
        "all" => selected.iter().map(|t| t.prefix.clone()).collect(),
        _ => updated.clone(),
    };
    // Sessions we deliberately leave alone still get a row with the reason —
    // the row IS the explanation, so nothing vanishes silently.
    let skipped_rows: Vec<SessionRestartStatus> = if restart_mode == "all" {
        Vec::new()
    } else {
        let reasons: Vec<(String, String)> = if restart_mode == "none" {
            selected.iter().map(|t| (t.prefix.clone(), "restart not requested".to_string())).collect()
        } else {
            not_updated.clone()
        };
        reasons
            .iter()
            .flat_map(|(prefix, why)| {
                app.restart_targets(std::slice::from_ref(prefix)).into_iter().map(move |(name, tool)| {
                    SessionRestartStatus {
                        session: name,
                        tool,
                        state: "skipped".into(),
                        message: Some(why.clone()),
                    }
                })
            })
            .collect()
    };
    let pending: Vec<SessionRestartStatus> = app
        .restart_targets(&restart_prefixes)
        .into_iter()
        .map(|(session, tool)| SessionRestartStatus {
            session,
            tool,
            state: "pending".into(),
            message: None,
        })
        .collect();
    app.with_update_job(|j| {
        j.phase = "restarting".into();
        j.sessions = pending.into_iter().chain(skipped_rows.into_iter()).collect();
    });

    app.restart_sessions(&restart_prefixes, |row| {
        let row = row.clone();
        app.with_update_job(|j| {
            if let Some(slot) = j.sessions.iter_mut().find(|s| s.session == row.session) {
                *slot = row;
            } else {
                j.sessions.push(row);
            }
        });
    });

    // C3 — a freshly installed CLI has no live sessions to restart (its `Create`
    // was refused by the 424), but it may well have RECORDED ones that Restore
    // has been skipping on every startup. `restore_all`'s own comment says
    // installing the CLI and restoring brings them back; this is the code that
    // performs that sentence's second half. "The CLI is back" vs "my work is back".
    if !installed.is_empty() {
        let (restored, failed) = app.restore_prefixes(&installed);
        app.with_update_job(|j| {
            for (session, tool) in restored {
                j.sessions.push(SessionRestartStatus {
                    session,
                    tool,
                    state: "resumed".into(),
                    message: Some("restored after install".into()),
                });
            }
            for (session, why) in failed {
                j.sessions.push(SessionRestartStatus {
                    session,
                    tool: String::new(),
                    state: "failed".into(),
                    message: Some(why),
                });
            }
        });
    }

    app.with_update_job(|j| {
        j.phase = "done".into();
        j.finished_at = Some(crate::engine::now_secs() as i64);
    });

    // C4 — availability changed, so re-advertise. A standalone agent needs
    // nothing (`/api/tools` live-probes per request); a hub caches the list from
    // `Register`, so without this the dashboard's "N missing" badge would stay
    // frozen at connect time and keep claiming a CLI we just installed is absent.
    if !installed.is_empty() {
        app.announce_tools();
    }
}

/// Update one tool's row in the live snapshot (no-op if the job was replaced).
fn set_tool_state(app: &AppState, prefix: &str, f: impl FnOnce(&mut UpdateToolStatus)) {
    app.with_update_job(|j| {
        if let Some(row) = j.tools.iter_mut().find(|r| r.tool == prefix) {
            f(row);
        }
    });
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
            image_paste: crate::tools::ImagePasteStrategy::ClipboardProbe,
            remote_off_flag: None,
            remote_on_flag: None,
        }
    }

    /// A scratch dir holding fake CLIs, prepended to the real PATH — the fakes
    /// shadow anything installed, while the shell still finds the coreutils the
    /// scripts use. `update_tool` then runs against this synthesized PATH exactly
    /// as it would against a session PATH.
    struct Bin(std::path::PathBuf, String);
    impl Bin {
        fn new(tag: &str) -> Bin {
            let d = std::env::temp_dir().join(format!("ccr-upd-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            let path = format!("{}:{}", d.display(), std::env::var("PATH").unwrap_or_default());
            Bin(d, path)
        }
        fn write(&self, name: &str, script: &str) {
            let p = self.0.join(name);
            std::fs::write(&p, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fn path(&self) -> &str {
            &self.1
        }
    }
    impl Drop for Bin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }


    // Proposal 0050 C2: with `installMissing`, a row that used to read
    // `skipped: not installed` and stop becomes `installing` → `installed`, while
    // a CLI that IS present stays on the update path — one loop, three branches.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_job_installs_what_is_missing_and_updates_what_is_there() {
        let b = Bin::new("job");
        let home = b.0.join("home");
        std::fs::create_dir_all(home.join(".local").join("bin")).unwrap();
        // The install drops the binary straight onto the session PATH.
        b.write(
            "do-install",
            &format!(
                "#!/bin/sh\nprintf '#!/bin/sh\\necho \"ghost 9.9\"\\n' > {d}/ghost-cli\nchmod 755 {d}/ghost-cli\n",
                d = b.0.display()
            ),
        );
        b.write("here-cli", "#!/bin/sh\necho 'here 1.0'\n");
        // A deliberately narrow session PATH: this box must look like one with
        // neither CLI, whatever the test host happens to have installed.
        let env_path = format!("{}:{}:/usr/bin:/bin", home.join(".local").join("bin").display(), b.0.display());

        let mut missing = tool("ghost", "ghost-cli");
        missing.install_hint = Some("do-install".into());
        let mut present = tool("here", "here-cli");
        present.update_cmd = Some("true".into()); // succeeds, moves nothing → `current`

        let tmp = b.0.join("cfg");
        std::fs::create_dir_all(&tmp).unwrap();
        let app = AppState::new(
            vec![missing.clone(), present.clone()],
            env_path.clone(),
            String::new(),
            tmp.clone(),
            home.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );

        // Without the flag: today's behaviour, byte for byte.
        start_job(&app, &UpdateReq { install_missing: false, ..Default::default() }).unwrap();
        let job = await_done(&app).await;
        let ghost = job.tools.iter().find(|t| t.tool == "ghost").unwrap();
        assert_eq!(ghost.state, "skipped");
        assert!(ghost.message.as_deref().unwrap_or("").contains("not installed"));
        assert!(!tools::binary_on_path("ghost-cli", &env_path), "nothing was installed");

        // With it: the same row installs, and availability is re-advertised.
        start_job(&app, &UpdateReq { install_missing: true, ..Default::default() }).unwrap();
        let job = await_done(&app).await;
        let ghost = job.tools.iter().find(|t| t.tool == "ghost").unwrap();
        assert_eq!(ghost.state, "installed", "{:?}", ghost.message);
        assert_eq!(ghost.to.as_deref(), Some("ghost 9.9"));
        assert!(tools::binary_on_path("ghost-cli", &env_path));
        // The tool that was already there took the UPDATE path, not the install one.
        let here = job.tools.iter().find(|t| t.tool == "here").unwrap();
        assert_eq!(here.state, "current");
        assert!(app.take_tools_dirty(), "an install must re-advertise the registry to the hub");
        assert!(!app.take_tools_dirty(), "…exactly once");

        let _ = std::fs::remove_dir_all(&b.0);
    }

    async fn await_done(app: &AppState) -> UpdateJob {
        for _ in 0..600 {
            let j = app.update_job();
            if j.phase == "done" {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job never finished: {:?}", app.update_job());
    }

    #[cfg(unix)]
    #[test]
    fn updated_when_the_version_actually_moves() {
        let b = Bin::new("moved");
        // The fake CLI reports the version recorded in a file; "updating" rewrites
        // that file — exactly the before/after shape a real updater produces.
        b.write(
            "fake-cli",
            "#!/bin/sh\nV=$(cat \"$(dirname \"$0\")/ver\" 2>/dev/null || echo 1.0.0)\necho \"fake-cli $V\"\n",
        );
        b.write("do-update", "#!/bin/sh\necho 2.0.0 > \"$(dirname \"$0\")/ver\"\n");
        std::fs::write(b.0.join("ver"), "1.0.0\n").unwrap();
        let mut t = tool("fake", "fake-cli");
        t.update_cmd = Some("do-update".into());

        assert_eq!(probe_version(&t, b.path()), "fake-cli 1.0.0");
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Updated { from, to } => {
                assert_eq!(from, "fake-cli 1.0.0");
                assert_eq!(to, "fake-cli 2.0.0");
            }
            other => panic!("expected Updated, got {other:?}"),
        }
        // Running it again is a no-op → `current`, not a second "updated".
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Current { version } => assert_eq!(version, "fake-cli 2.0.0"),
            other => panic!("expected Current, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn opencode_upgrade_uses_version_probe_verdict() {
        let b = Bin::new("opencode");
        b.write("opencode", "#!/bin/sh\nD=$(dirname \"$0\")\nif [ \"$1\" = --version ]; then cat \"$D/ver\"; elif [ \"$1\" = upgrade ]; then echo 'opencode 1.18.23' > \"$D/ver\"; fi\n");
        std::fs::write(b.0.join("ver"), "opencode 1.18.22\n").unwrap();
        let tools = crate::tools::load_tools(None, &b.0);
        let tool = tools.iter().find(|t| t.prefix == "opencode").unwrap();
        assert_eq!(crate::tools::update_commands(tool), vec!["opencode upgrade"]);
        assert!(matches!(update_tool(tool, b.path(), &b.0), UpdateOutcome::Updated { .. }));
        assert!(matches!(update_tool(tool, b.path(), &b.0), UpdateOutcome::Current { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn grok_update_uses_version_probe_and_restores_rc() {
        let b = Bin::new("grok-upd");
        let home = b.0.join("home");
        std::fs::create_dir_all(home.join(".local").join("bin")).unwrap();
        std::fs::write(home.join(".bashrc"), "keep\n").unwrap();
        b.write(
            "grok",
            &format!(
                "#!/bin/sh\nD=$(dirname \"$0\")\nif [ \"$1\" = --version ]; then cat \"$D/ver\"; elif [ \"$1\" = update ]; then echo mutated >> \"{h}/.bashrc\"; echo 'grok 1.0.14' > \"$D/ver\"; fi\n",
                h = home.display()
            ),
        );
        std::fs::write(b.0.join("ver"), "grok 1.0.13\n").unwrap();
        let tools = crate::tools::load_tools(None, &b.0);
        let tool = tools.iter().find(|t| t.prefix == "grok").unwrap();
        assert_eq!(crate::tools::update_commands(tool), vec!["grok update"]);
        match update_tool(tool, b.path(), &home) {
            UpdateOutcome::Updated { from, to } => {
                assert!(from.contains("1.0.13"), "{from}");
                assert!(to.contains("1.0.14"), "{to}");
            }
            other => panic!("expected Updated, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(home.join(".bashrc")).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_successful_noop_is_current_not_updated() {
        // The crux of the design: the updater exits 0 having changed nothing.
        // Trusting the exit code would report "updated"; the re-probe reports the
        // truth.
        let b = Bin::new("noop");
        b.write("fake-cli", "#!/bin/sh\necho 'fake-cli 1.0.0'\n");
        let mut t = tool("fake", "fake-cli");
        t.update_cmd = Some("true".into());
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Current { version } => assert_eq!(version, "fake-cli 1.0.0"),
            other => panic!("expected Current, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn failure_reports_the_commands_own_error() {
        let b = Bin::new("fail");
        b.write("fake-cli", "#!/bin/sh\necho 'fake-cli 1.0.0'\n");
        b.write("do-update", "#!/bin/sh\necho 'npm ERR! EACCES permission denied' >&2\nexit 13\n");
        let mut t = tool("fake", "fake-cli");
        t.update_cmd = Some("do-update".into());
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Failed { version, error } => {
                assert_eq!(version, "fake-cli 1.0.0", "the row still shows what's installed");
                assert!(error.contains("EACCES"), "actionable error text: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn falls_through_to_the_next_candidate() {
        // A self-updater that can't act on this install (fails, version unmoved)
        // must not end the attempt — the package-manager fallback runs next.
        let b = Bin::new("fallthrough");
        b.write(
            "fake-cli",
            "#!/bin/sh\nV=$(cat \"$(dirname \"$0\")/ver\" 2>/dev/null || echo 1.0.0)\necho \"$V\"\n",
        );
        b.write("self-update", "#!/bin/sh\necho 'installed via npm; use npm' >&2\nexit 1\n");
        b.write("pkg-update", "#!/bin/sh\necho 3.0.0 > \"$(dirname \"$0\")/ver\"\n");
        std::fs::write(b.0.join("ver"), "1.0.0\n").unwrap();
        // Two candidates, in order — the shape `update_commands` produces.
        let mut t = tool("fake", "fake-cli");
        t.update_cmd = Some("self-update".into());
        // Simulate the registry's second candidate by chaining: the runner tries
        // each entry of update_commands in order, so exercise it via a compound
        // override that fails, then a real one.
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Failed { .. } => {}
            other => panic!("a lone failing candidate is Failed, got {other:?}"),
        }
        t.update_cmd = Some("self-update || pkg-update".into());
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Updated { from, to } => {
                assert_eq!((from.as_str(), to.as_str()), ("1.0.0", "3.0.0"));
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_cli_and_unknown_command_are_skipped_not_failed() {
        let b = Bin::new("skip");
        // Not installed at all.
        let t = tool("ghost", "ghost-cli");
        match update_tool(&t, b.path(), &b.0) {
            UpdateOutcome::Skipped { reason } => assert!(reason.contains("not installed"), "{reason}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Installed, but a custom tool with no descriptor and no cc_tool_update.
        b.write("plain-cli", "#!/bin/sh\necho 1.0\n");
        let t2 = tool("plain", "plain-cli");
        match update_tool(&t2, b.path(), &b.0) {
            UpdateOutcome::Skipped { reason } => assert!(reason.contains("no known update command"), "{reason}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_command_is_killed_at_the_timeout() {
        let b = Bin::new("hang");
        b.write("fake-cli", "#!/bin/sh\necho 'fake-cli 1.0.0'\n");
        let mut t = tool("fake", "fake-cli");
        t.update_cmd = Some("sleep 30".into());
        std::env::set_var("CCWEB_UPDATE_TIMEOUT_SECS", "1");
        let started = Instant::now();
        let out = update_tool(&t, b.path(), &b.0);
        std::env::remove_var("CCWEB_UPDATE_TIMEOUT_SECS");
        assert!(started.elapsed() < Duration::from_secs(15), "the timeout must bound the job");
        match out {
            UpdateOutcome::Failed { error, .. } => assert!(error.contains("timed out"), "{error}"),
            other => panic!("expected Failed(timed out), got {other:?}"),
        }
    }
}
