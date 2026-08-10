// cc-screen-tui (`ccs`) — terminal client for the cc-screen-rust backend.
// M1: session switcher (list + navigate). Attach/input/lifecycle land in M2–M4.
//
// The modules live in the crate's library (`src/lib.rs`) so the e2e harness can
// drive a real `App`; this binary is a thin shell over them (proposal 0059 B2).

use anyhow::{Context, Result};
use clap::Parser;

use cc_screen_tui::{actions, app, cli, client, config, term};

#[tokio::main]
async fn main() -> Result<()> {
    // `ccs update` / `ccs uninstall` / `ccs activate` (alias `login`) /
    // `ccs logout` are bare positionals handled before clap (which would
    // otherwise reject them). Unlike the agent/hub, ccs has no service — the
    // binary itself is the install. A session literally named like one of these
    // is unreachable by direct-attach (same shadowing rule as `update`).
    sweep_parked_exe(); // clear what a previous `ccs update` left behind (Windows)
    match std::env::args().nth(1).as_deref() {
        Some("update") => return run_update(),
        Some("uninstall") => return run_uninstall(),
        Some("activate") | Some("login") => return run_activate_cmd().await,
        Some("logout") => return run_logout_cmd().await,
        _ => {}
    }
    let cli = cli::Cli::parse();
    let mut cfg = config::Config::load();

    // Honest persistence (0060 A1): the *effective* server becomes the config
    // truth before the app ever saves (the recents-save used to clobber an
    // explicit --server back to the stale loaded value). An explicit
    // --server/--token is persisted immediately — the documented "plain `ccs`
    // reconnects" behavior — with the token in the 0600 credentials store, not
    // config.toml.
    let server = cli.server.clone().unwrap_or_else(|| cfg.server.clone());
    cfg.server = server.clone();
    if cli.server.is_some() {
        cfg.save()?;
    }
    if let Some(t) = cli.token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        config::store_credential(&server, t)?;
    }
    let mut server = server;
    let mut token = config::resolve_token(
        cli.token.clone(),
        std::env::var("CCS_API_TOKEN").ok(),
        std::env::var("CCWEB_API_TOKEN").ok(),
        config::credential_for(&server),
        cfg.api_token.clone(),
    );

    // Not signed in yet → plain `ccs` IS `ccs activate` (0060 follow-up): with
    // no token from any source and an interactive terminal, run the device
    // sign-in right here — against the saved server if it's a multi-tenant
    // hub, else the hosted hub — persist, and continue straight into the TUI.
    // An explicit --server that isn't a hub (a direct agent, an open
    // self-host) is left alone; so is any non-TTY/scripted run.
    if token.is_none() && stdin_is_tty() {
        let target = if cli.server.is_some() {
            match actions::probe_multi_tenant(&server, cli.insecure).await {
                Ok(true) => Some(server.clone()),
                _ => None, // explicit non-hub server: tokenless is legitimate
            }
        } else {
            Some(actions::resolve_activate_server(Some(&server), cli.insecure).await)
        };
        if let Some(target) = target {
            println!("No sign-in yet — running `ccs activate` first.\n");
            let mut out = std::io::stdout();
            match actions::run_activate(&target, cli.insecure, &default_label(), true, &mut out).await {
                Ok(a) => {
                    actions::persist_login(&target, &a.token)?;
                    println!("\n{} Logged in as {}\n", actions::check_mark(), a.email);
                    server = target;
                    cfg.server = server.clone();
                    token = Some(a.token);
                }
                Err(e) => {
                    eprintln!("ccs: {}", e.message());
                    std::process::exit(e.exit_code());
                }
            }
        }
    }

    let rest = client::Rest::new(&server, cli.insecure, token)?;

    // `ccs <session>` direct-attach pre-flight (0059 C2). Resolve BEFORE entering
    // the alt-screen so any error is visible on the normal screen with a real exit
    // code: not found → 1, ambiguous → 2 (candidates on stderr). On success we hand
    // the raw query to the app, which re-resolves it against the same list at boot.
    // (A session name equal to `update`/`uninstall` is unreachable here — those are
    // consumed as bare positionals above — so a collision can't hijack this path.)
    if let Some(query) = cli.attach.clone() {
        let sessions = rest.sessions().await.unwrap_or_else(|e| {
            let first = e.to_string().lines().next().unwrap_or("").to_string();
            eprintln!("ccs: couldn't list sessions from {server}: {first}");
            std::process::exit(1);
        });
        match app::resolve_attach(&sessions, &query) {
            Ok(_) => {}
            Err(app::AttachError::NotFound) => {
                eprintln!("ccs: no session matches '{query}'");
                std::process::exit(1);
            }
            Err(app::AttachError::Ambiguous(cands)) => {
                eprintln!("ccs: '{query}' is ambiguous — {} candidates:", cands.len());
                for c in &cands {
                    eprintln!("  {c}");
                }
                std::process::exit(2);
            }
        }
    }

    let mut term = term::enter()?;
    let mut app = app::App::new(rest, cfg);
    if let Some(query) = cli.attach {
        app.set_start_attach(query);
    }
    let res = app.run(&mut term).await;
    // Always restore the terminal, even if the app loop errored.
    let _ = term::restore();
    res
}

/// The tiny flag parse the pre-clap subcommands share: `--server <url>` (or
/// `--server=<url>`) and `--insecure`. These arms never reach clap — which is
/// also what keeps the 0059 `ccs <session>` positional unambiguous.
fn parse_sub_flags() -> (Option<String>, bool) {
    let mut server = None;
    let mut insecure = false;
    let mut args = std::env::args().skip(2);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--server" => server = args.next(),
            "--insecure" => insecure = true,
            other => {
                if let Some(v) = other.strip_prefix("--server=") {
                    server = Some(v.to_string());
                }
            }
        }
    }
    (server.map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()), insecure)
}

fn stdin_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// The default client label: `user@hostname` — shown on the approve page and
/// the account page's "Terminal clients" list.
fn default_label() -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "terminal".into());
    format!("{user}@{host}")
}

/// `ccs activate` (alias `login`) — device-code sign-in to a multi-tenant hub
/// (proposal 0060 Part C). Runs pre-TTY on the normal screen; exit codes: 0
/// signed in, 1 denied/expired/failed, 2 this-hub-can't (single-tenant or
/// pre-0060).
async fn run_activate_cmd() -> Result<()> {
    let (flag_server, insecure) = parse_sub_flags();
    let server = match flag_server {
        Some(s) => s,
        // Flag-free: the saved server if it's a real multi-tenant hub, else
        // the hosted hub. Never a prompt, never a dead end.
        None => {
            let saved = config::config_path()
                .map(|p| p.exists())
                .unwrap_or(false)
                .then(|| config::Config::load().server)
                .filter(|s| !s.trim().is_empty());
            actions::resolve_activate_server(saved.as_deref(), insecure).await
        }
    };
    let mut out = std::io::stdout();
    match actions::run_activate(&server, insecure, &default_label(), true, &mut out).await {
        Ok(a) => {
            actions::persist_login(&server, &a.token)?;
            let where_ = config::credentials_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "the credentials store".into());
            println!("\n{} Logged in as {}", actions::check_mark(), a.email);
            println!("  Saved to {where_} — run `ccs` to connect.");
            Ok(())
        }
        Err(e) => {
            eprintln!("ccs: {}", e.message());
            std::process::exit(e.exit_code());
        }
    }
}

/// `ccs logout` — revoke this terminal's client token server-side (best
/// effort) and drop the local credentials entry (always).
async fn run_logout_cmd() -> Result<()> {
    let (flag_server, insecure) = parse_sub_flags();
    let server = flag_server.unwrap_or_else(|| config::Config::load().server.trim_end_matches('/').to_string());
    let outcome = actions::run_logout(&server, insecure).await?;
    let host = config::host_key(&server);
    if !outcome.had_credential {
        println!("ccs: no stored sign-in for {host} — nothing to do.");
        return Ok(());
    }
    println!("{} Logged out of {host}", actions::check_mark());
    if !outcome.revoked {
        println!("  (couldn't confirm the server-side revocation — you can also revoke it from the account page)");
    }
    Ok(())
}

/// The installer asset for this platform. `ccs` ships from the `cc-screen-tui`
/// package, so cargo-dist names them `cc-screen-tui-installer.{sh,ps1}`.
fn installer_url(windows: bool) -> String {
    let asset = if windows { "cc-screen-tui-installer.ps1" } else { "cc-screen-tui-installer.sh" };
    format!("{}/{asset}", cc_screen_protocol::RELEASE_BASE_URL)
}

/// `(program, args)` that fetches and runs the installer at `url`.
///
/// The PowerShell leg carries `-ExecutionPolicy Bypass` **for that child process
/// only** — nothing on the machine changes. Without it cargo-dist's installer
/// refuses outright on a default Windows desktop, whose policy blocks scripts;
/// asking a user to loosen a machine-wide setting to run `ccs update` would be a
/// far bigger ask than the update itself.
fn installer_cmd(url: &str, windows: bool) -> (&'static str, Vec<String>) {
    if windows {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-Command".into(),
                format!("irm '{url}' | iex"),
            ],
        )
    } else {
        (
            "sh",
            vec!["-c".into(), format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh")],
        )
    }
}

/// The paste-able one-liner that (re)installs ccs on this platform.
fn reinstall_hint(windows: bool) -> String {
    let url = installer_url(windows);
    if windows {
        format!("powershell -ExecutionPolicy Bypass -Command \"irm {url} | iex\"")
    } else {
        format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh")
    }
}

/// Where a running `ccs.exe` is parked while it replaces itself: a sibling with
/// a `.old` suffix, so the sweep at startup finds it without guessing.
fn parked_path(exe: &std::path::Path) -> std::path::PathBuf {
    let mut s = exe.as_os_str().to_os_string();
    s.push(".old");
    s.into()
}

/// Delete the copy a previous `ccs update` parked. Best-effort and silent: the
/// file is only ever a stale binary, and if it is somehow still locked the next
/// start tries again. No-op off Windows, where nothing is ever parked.
fn sweep_parked_exe() {
    if !cfg!(windows) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(parked_path(&exe));
    }
}

/// `ccs update` — re-run the hosted installer to fetch the latest `ccs` binary.
/// The TUI has no service to restart.
///
/// On Windows the running `.exe` can't be *overwritten* — but it can be
/// *renamed*, so we move ourselves aside first and let the installer write a
/// fresh `ccs.exe`; [`sweep_parked_exe`] clears the leftover on the next start.
/// If the install fails we move the parked copy back: an update that can't
/// fetch a new build must never leave the machine with no `ccs` at all.
fn run_update() -> Result<()> {
    let windows = cfg!(windows);
    let url = installer_url(windows);
    println!("→ downloading the latest ccs from {url}");

    let parked = if windows { park_running_exe()? } else { None };
    let (prog, args) = installer_cmd(&url, windows);
    let ran = std::process::Command::new(prog).args(&args).status();
    let installed = matches!(&ran, Ok(s) if s.success())
        && parked.as_ref().is_none_or(|(exe, _)| exe.exists());

    if !installed {
        if let Some((exe, parked)) = &parked {
            let _ = std::fs::rename(parked, exe); // put the working binary back
        }
        match ran {
            Ok(_) => anyhow::bail!("installer failed (is the site reachable?)"),
            Err(e) => anyhow::bail!("couldn't run the installer ({prog}): {e}"),
        }
    }
    println!("✓ updated ccs. Re-run `ccs` to use the new build.");
    Ok(())
}

/// Move the running executable aside so a fresh one can be written in its place,
/// returning `(original, parked)`.
fn park_running_exe() -> Result<Option<(std::path::PathBuf, std::path::PathBuf)>> {
    let exe = std::env::current_exe().context("can't locate the running ccs binary")?;
    let parked = parked_path(&exe);
    let _ = std::fs::remove_file(&parked); // leftover from an earlier update
    std::fs::rename(&exe, &parked)
        .with_context(|| format!("couldn't move {} aside to update it", exe.display()))?;
    Ok(Some((exe, parked)))
}

/// The detached helper that removes the running binary once we've exited —
/// Windows refuses to delete a running program, so the deletion has to outlive
/// us. Single quotes are PowerShell's literal string; a `'` in the path is
/// escaped by doubling it.
fn self_delete_cmd(exe: &std::path::Path) -> (&'static str, Vec<String>) {
    let path = exe.display().to_string().replace('\'', "''");
    (
        "powershell",
        vec![
            "-NoProfile".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            format!(
                "Start-Sleep -Milliseconds 1500; \
                 Remove-Item -LiteralPath '{path}' -Force -ErrorAction SilentlyContinue"
            ),
        ],
    )
}

/// `ccs uninstall` — remove the installed binary and its config. ccs runs no
/// service, so the binary *is* the install: we unlink the running executable
/// (safe while running on Unix — the inode lives until the process exits) and
/// drop the config dir. On Windows the unlink can't happen from inside the
/// running program, so a detached helper does it a moment after we exit.
/// Re-install anytime via the hosted one-liner.
fn run_uninstall() -> Result<()> {
    // Config dir first (parent of config.toml) — best-effort, absence is fine.
    if let Some(dir) = config::config_path().as_ref().and_then(|p| p.parent()) {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => println!("→ removed config {}", dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("  (left config {}: {e})", dir.display()),
        }
    }
    let exe = std::env::current_exe().context("can't locate the ccs binary to remove")?;
    if cfg!(windows) {
        let (prog, args) = self_delete_cmd(&exe);
        std::process::Command::new(prog)
            .args(&args)
            .spawn()
            .with_context(|| format!("couldn't schedule the removal of {}", exe.display()))?;
        println!("✓ removing ccs ({}) — done a moment after this exits", exe.display());
    } else {
        std::fs::remove_file(&exe)
            .with_context(|| format!("couldn't remove {}", exe.display()))?;
        println!("✓ removed ccs ({})", exe.display());
    }
    println!("  Re-install: {}", reinstall_hint(cfg!(windows)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both legs are built from the same inputs, so the Windows shapes are
    /// asserted from any host — the platform only picks which one runs.
    #[test]
    fn installer_url_picks_the_platform_asset() {
        assert!(installer_url(true).ends_with("/cc-screen-tui-installer.ps1"));
        assert!(installer_url(false).ends_with("/cc-screen-tui-installer.sh"));
        assert!(installer_url(true).starts_with(cc_screen_protocol::RELEASE_BASE_URL));
    }

    #[test]
    fn windows_installer_runs_under_a_bypass_policy() {
        let (prog, args) = installer_cmd("https://x/i.ps1", true);
        assert_eq!(prog, "powershell");
        // The Bypass is what makes this work on a stock desktop; if it ever
        // disappears, `ccs update` silently regresses to "refused to run".
        assert!(args.windows(2).any(|w| w == ["-ExecutionPolicy", "Bypass"]), "{args:?}");
        assert_eq!(args.last().unwrap(), "irm 'https://x/i.ps1' | iex");
    }

    #[test]
    fn unix_installer_is_unchanged() {
        let (prog, args) = installer_cmd("https://x/i.sh", false);
        assert_eq!(prog, "sh");
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "curl --proto '=https' --tlsv1.2 -LsSf https://x/i.sh | sh");
    }

    #[test]
    fn parked_binary_is_a_sibling_of_the_original() {
        let exe = std::path::Path::new("/home/u/.local/bin/ccs.exe");
        let parked = parked_path(exe);
        assert_eq!(parked.parent(), exe.parent(), "must stay on the same volume to rename");
        assert_eq!(parked.file_name().unwrap(), "ccs.exe.old");
    }

    #[test]
    fn self_delete_targets_the_binary_and_escapes_quotes() {
        let (prog, args) = self_delete_cmd(std::path::Path::new(r"C:\Users\o'brien\ccs.exe"));
        assert_eq!(prog, "powershell");
        let script = args.last().unwrap();
        assert!(script.contains("Start-Sleep"), "must outlive this process: {script}");
        // Doubled, so PowerShell reads one literal apostrophe.
        assert!(script.contains(r"C:\Users\o''brien\ccs.exe"), "{script}");
    }

    #[test]
    fn reinstall_hint_is_paste_able_per_platform() {
        assert!(reinstall_hint(false).starts_with("curl "));
        let win = reinstall_hint(true);
        assert!(win.starts_with("powershell -ExecutionPolicy Bypass"), "{win}");
        assert!(win.contains(".ps1"), "{win}");
    }
}
