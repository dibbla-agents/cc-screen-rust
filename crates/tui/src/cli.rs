use clap::Parser;

/// Terminal client for cc-screen-rust — your AI coding sessions in a switcher + grid.
///
/// Point `--server` at one machine's agent (e.g. http://laptop:8839) to drive that
/// box's sessions, OR at a hub (e.g. http://hub:8840) to see EVERY connected
/// machine's sessions in one list, each tagged with its machine. Same binary either
/// way — the hub just aggregates.
///
/// Against a hosted (multi-tenant) hub, sign in once with `ccs activate`. The server
/// URL is remembered in ~/.config/cc-screen-tui/config.toml and sign-ins/tokens in
/// the sibling credentials.toml (0600), so after the first run `ccs` (no args)
/// reconnects. Inside the grid, the prefix key is Ctrl-A (tmux-style): Ctrl-A d
/// opens the menu, Ctrl-A then a layout digit, etc.
#[derive(Parser, Debug)]
#[command(
    name = "ccs",
    version,
    about,
    long_about,
    after_help = "EXAMPLES:\n  \
        ccs activate                                    # sign in to a hosted hub (device code)\n  \
        ccs --server http://laptop:8839                 # one machine\n  \
        ccs --server http://hub:8840 --token <tok>      # a self-hosted hub (static token)\n  \
        ccs                                             # reuse the saved server + sign-in\n  \
        ccs logout                                      # revoke this terminal's sign-in\n  \
        ccs update                                      # fetch the latest ccs build\n  \
        ccs uninstall                                   # remove the ccs binary + config\n\n\
        Auth: against a hosted (multi-tenant) hub, run `ccs activate` — approve the\n  \
        one-time code from any logged-in browser (your phone works). Against a\n  \
        self-hosted server/hub with a static gate, pass --token (or set api_token in\n  \
        the config, or CCS_API_TOKEN / CCWEB_API_TOKEN). Sign-ins are stored per hub\n  \
        in ~/.config/cc-screen-tui/credentials.toml (0600)."
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

    /// Session to attach on start (0059 C2): `ccs alpha` boots straight into the
    /// grid attached to that session instead of the action menu. Accepts an exact
    /// `machine/name`, an exact `name`, a unique name prefix, or a unique fuzzy
    /// match. Ambiguous prints the candidates and exits 2; not found exits 1.
    #[arg(value_name = "SESSION")]
    pub attach: Option<String>,
}
