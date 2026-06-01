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
    let cli = cli::Cli::parse();
    let cfg = config::Config::load();
    let server = cli.server.clone().unwrap_or_else(|| cfg.server.clone());

    let rest = client::Rest::new(&server, cli.insecure)?;

    let mut term = term::enter()?;
    let app = app::App::new(rest, cfg);
    let res = app.run(&mut term).await;
    // Always restore the terminal, even if the app loop errored.
    let _ = term::restore();
    res
}
