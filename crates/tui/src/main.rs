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

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    // `ccs update` re-runs the hosted installer to fetch the latest build. Handled
    // before clap (which would reject the bare positional).
    if std::env::args().nth(1).as_deref() == Some("update") {
        return run_update();
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
