// cc-screen-tui (`ccs`) — terminal client for the cc-screen-rust backend.
// M1: session switcher (list + navigate). Attach/input/lifecycle land in M2–M4.
//
// The modules live in the crate's library (`src/lib.rs`) so the e2e harness can
// drive a real `App`; this binary is a thin shell over them (proposal 0059 B2).

use anyhow::{Context, Result};
use clap::Parser;

use cc_screen_tui::{app, cli, client, config, term};

#[tokio::main]
async fn main() -> Result<()> {
    // `ccs update` / `ccs uninstall` are bare positionals handled before clap
    // (which would otherwise reject them). Unlike the agent/hub, ccs has no
    // service — the binary itself is the install.
    match std::env::args().nth(1).as_deref() {
        Some("update") => return run_update(),
        Some("uninstall") => return run_uninstall(),
        _ => {}
    }
    let cli = cli::Cli::parse();
    let cfg = config::Config::load();
    let server = cli.server.clone().unwrap_or_else(|| cfg.server.clone());
    let token = config::resolve_token(
        cli.token.clone(),
        std::env::var("CCS_API_TOKEN").ok(),
        std::env::var("CCWEB_API_TOKEN").ok(),
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
