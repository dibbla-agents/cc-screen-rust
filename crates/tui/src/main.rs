// cc-screen-tui (`ccs`) — terminal client for the cc-screen-rust backend.
// M1: session switcher (list + navigate). Attach/input/lifecycle land in M2–M4.

mod app;
mod cli;
mod client;
mod config;
mod input;
mod layout;
mod pane;
mod term;
mod ui;

use anyhow::{Context, Result};
use clap::Parser;

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

    let mut term = term::enter()?;
    let app = app::App::new(rest, cfg);
    let res = app.run(&mut term).await;
    // Always restore the terminal, even if the app loop errored.
    let _ = term::restore();
    res
}

/// `ccs update` — re-run the hosted installer (same `curl | sh` the docs site
/// serves) to fetch the latest `ccs` binary. The TUI has no service to restart.
fn run_update() -> Result<()> {
    let url = format!("{}/install-ccs.sh", cc_screen_protocol::RELEASE_BASE_URL);
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
        "  Re-install: curl --proto '=https' --tlsv1.2 -LsSf {}/install-ccs.sh | sh",
        cc_screen_protocol::RELEASE_BASE_URL
    );
    Ok(())
}
