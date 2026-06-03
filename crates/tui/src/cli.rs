use clap::Parser;

/// Terminal client for cc-screen-rust — your AI coding sessions in a switcher + grid.
///
/// Point `--server` at one machine's agent (e.g. http://laptop:8839) to drive that
/// box's sessions, OR at a hub (e.g. http://hub:8840) to see EVERY connected
/// machine's sessions in one list, each tagged with its machine. Same binary either
/// way — the hub just aggregates.
///
/// The server URL + token are remembered in ~/.config/cc-screen-tui/config.toml, so
/// after the first run `ccs` (no args) reconnects. Inside the grid, the prefix key
/// is Ctrl-A (tmux-style): Ctrl-A d opens the menu, Ctrl-A then a layout digit, etc.
#[derive(Parser, Debug)]
#[command(
    name = "ccs",
    version,
    about,
    long_about,
    after_help = "EXAMPLES:\n  \
        ccs --server http://laptop:8839                 # one machine\n  \
        ccs --server http://hub:8840 --token <tok>      # a hub (all machines)\n  \
        ccs                                             # reuse the saved server/token\n  \
        ccs update                                      # fetch the latest ccs build\n\n\
        Auth: if the server/hub has a gate, pass --token (or set api_token in the\n  \
        config, or CCS_API_TOKEN / CCWEB_API_TOKEN). The browser uses a login screen;\n  \
        the TUI uses the token directly."
)]
pub struct Cli {
    /// Server/hub base URL, e.g. http://laptop:8839 or http://hub:8840. Overrides
    /// the config file; remembered for next time.
    #[arg(long)]
    pub server: Option<String>,

    /// API token for an auth-gated server/hub (overrides config `api_token` and
    /// CCS_API_TOKEN / CCWEB_API_TOKEN). This is the CLIENT token, not an agent's
    /// hub-uplink token.
    #[arg(long)]
    pub token: Option<String>,

    /// Accept invalid TLS certificates (for an ad-hoc self-signed `wss`).
    #[arg(long)]
    pub insecure: bool,
}
