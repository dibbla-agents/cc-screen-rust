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
    match std::env::args().nth(1).as_deref() {
        Some("update") => return run_update(),
        Some("uninstall") => return run_uninstall(),
        Some("activate") | Some("login") => return run_activate_cmd().await,
        Some("logout") => return run_logout_cmd().await,
        _ => {}
    }
    let cli = cli::Cli::parse();
    let had_config = config::config_path().map(|p| p.exists()).unwrap_or(false);
    let mut cfg = config::Config::load();

    // First-run banner (0060 C5): no config file at all, no --server, an
    // interactive terminal → ask once where to connect, and roll straight into
    // the device sign-in when the hub turns out to be multi-tenant. Strictly
    // gated on no-config-exists: a config with a dead/unauthed server gets the
    // one-line 401 hint, never a wizard.
    if !had_config && cli.server.is_none() && cli.attach.is_none() && stdin_is_tty() {
        if let Some(server) = first_run_prompt()? {
            cfg.server = server.clone();
            cfg.save()?;
            match actions::probe_multi_tenant(&server, cli.insecure).await {
                Ok(true) => {
                    let mut out = std::io::stdout();
                    match actions::run_activate(&server, cli.insecure, &default_label(), true, &mut out).await {
                        Ok(a) => {
                            actions::persist_login(&server, &a.token)?;
                            println!("\n{} Logged in as {}", actions::check_mark(), a.email);
                        }
                        Err(e) => {
                            eprintln!("ccs: {}", e.message());
                            std::process::exit(e.exit_code());
                        }
                    }
                }
                Ok(false) => {} // reachable, no device flow — continue into the TUI
                Err(e) => eprintln!("ccs: {} (continuing — the TUI will keep retrying)", e.message()),
            }
        }
    }

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
    let token = config::resolve_token(
        cli.token.clone(),
        std::env::var("CCS_API_TOKEN").ok(),
        std::env::var("CCWEB_API_TOKEN").ok(),
        config::credential_for(&server),
        cfg.api_token.clone(),
    );

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

/// C5's one-time question. `Ok(None)` means the user gave an empty-after-trim
/// answer equal to nothing we can use (never happens — empty takes the default).
fn first_run_prompt() -> Result<Option<String>> {
    use std::io::Write;
    println!("No server configured.");
    print!("Server URL [https://app.ccscreen.dev]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim().trim_end_matches('/');
    Ok(Some(if s.is_empty() { "https://app.ccscreen.dev".to_string() } else { s.to_string() }))
}

/// `ccs activate` (alias `login`) — device-code sign-in to a multi-tenant hub
/// (proposal 0060 Part C). Runs pre-TTY on the normal screen; exit codes: 0
/// signed in, 1 denied/expired/failed, 2 this-hub-can't (single-tenant or
/// pre-0060).
async fn run_activate_cmd() -> Result<()> {
    let (flag_server, insecure) = parse_sub_flags();
    let had_config = config::config_path().map(|p| p.exists()).unwrap_or(false);
    let server = match flag_server {
        Some(s) => s,
        None if had_config => config::Config::load().server.trim_end_matches('/').to_string(),
        None => {
            if !stdin_is_tty() {
                eprintln!("ccs: no server configured — pass --server <url>");
                std::process::exit(2);
            }
            first_run_prompt()?.unwrap_or_else(|| "https://app.ccscreen.dev".into())
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

/// `ccs update` — re-run the hosted installer (same `curl | sh` the GitHub
/// Release serves) to fetch the latest `ccs` binary. The TUI has no service to
/// restart. `ccs` ships from the `cc-screen-tui` package, so its installer asset
/// is `cc-screen-tui-installer.sh`.
fn run_update() -> Result<()> {
    let url = format!("{}/cc-screen-tui-installer.sh", cc_screen_protocol::RELEASE_BASE_URL);
    println!("→ downloading the latest ccs from {url}");
    let cmd = format!("curl --proto '=https' --tlsv1.2 -LsSf {url} | sh");
    let status = std::process::Command::new("sh").arg("-c").arg(&cmd).status()?;
    if !status.success() {
        anyhow::bail!("installer failed (is curl available, and the site reachable?)");
    }
    println!("✓ updated ccs. Re-run `ccs` to use the new build.");
    Ok(())
}

/// `ccs uninstall` — remove the installed binary and its config. ccs runs no
/// service, so the binary *is* the install: we unlink the running executable
/// (safe while running on Unix — the inode lives until the process exits) and
/// drop `~/.config/cc-screen-tui`. Re-install anytime via the hosted one-liner.
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
    std::fs::remove_file(&exe).with_context(|| format!("couldn't remove {}", exe.display()))?;
    println!("✓ removed ccs ({})", exe.display());
    println!(
        "  Re-install: curl --proto '=https' --tlsv1.2 -LsSf {}/cc-screen-tui-installer.sh | sh",
        cc_screen_protocol::RELEASE_BASE_URL
    );
    Ok(())
}
