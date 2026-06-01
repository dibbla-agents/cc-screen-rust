use clap::Parser;

/// Terminal client for cc-screen-rust — list sessions and attach to one.
#[derive(Parser, Debug)]
#[command(name = "ccs", version, about)]
pub struct Cli {
    /// Server base URL (overrides the config file). e.g. http://127.0.0.1:8839
    #[arg(long)]
    pub server: Option<String>,

    /// Accept invalid TLS certificates (for an ad-hoc self-signed `wss`).
    #[arg(long)]
    pub insecure: bool,
}
