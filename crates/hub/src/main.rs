// cc-screen-hub — the aggregator binary. A thin wrapper over the crate library
// (see lib.rs): parse args, wire up HubState, build the router, serve. Agents dial
// in over `/agent/ws` and register; clients speak the same wire contract they'd
// speak to a single agent, and the hub routes each request to the owning machine.
// The hub owns NO PTY and NO filesystem — it is a registry + client-auth gate +
// transparent byte relay.

use std::sync::Arc;

use cc_screen_auth::Auth;
use cc_screen_hub::{build_router, config, registry::Registry, service, state::HubState};

/// Runtime usage. Service setup is `cc-screen-hub install --help`.
fn print_usage() {
    println!(
        r#"cc-screen-hub — the aggregator: one address in front of many machines. Agents
(cc-screen-rust, run with --hub) dial IN and register; you point your browser or
the `ccs` TUI at the hub and see every machine's sessions in one list. The hub
owns no PTYs and no files — it relays to the owning agent.

USAGE
  cc-screen-hub [--addr HOST:PORT]     run the hub
  cc-screen-hub install [--help]       set it up as an auto-starting service (usual way)
  cc-screen-hub update                 fetch the latest release + restart the service
  cc-screen-hub uninstall              remove that service

RUN-DIRECTLY FLAGS
  --addr HOST:PORT    bind address (default 127.0.0.1:8840; env CCWEB_ADDR)

CONFIG (env / ~/.config/cc-screen-hub/web.env)
  CCWEB_PASSWORD / CCWEB_API_TOKEN   client auth gate (the browser/TUI login)
  CCHUB_AGENT_TOKENS                 per-agent uplink tokens, "machine:token,m2:tok2".
                                     Empty = OPEN uplink (any agent may register); the
                                     hub refuses to start in that case (even on loopback —
                                     it may be tunnel-fronted) unless CCHUB_ALLOW_OPEN_UPLINK=1.
                                     Set tokens to require known agents.
  CCWEB_CONFIG_DIR                   override the state dir (default ~/.config/cc-screen-hub)
                                     so a second hub (e.g. a test instance on another
                                     port) runs with fully isolated state.

SETUP
  1. On the hub box:   cc-screen-hub install --password PW --agents 'laptop:T1,server:T2'
  2. On each machine:  cc-screen-rust install --hub https://HUB:8840 --hub-token T1 --machine-id laptop
  3. Open the hub URL in a browser, or:  ccs --server https://HUB:8840 --token <client-token>

Off-tailnet: front the hub with a TLS reverse proxy and always set CCHUB_AGENT_TOKENS."#
    );
}

#[tokio::main]
async fn main() {
    // `install` / `uninstall` wire up (or tear down) the hub's own service and
    // exit — no server, no tracing.
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("install") => {
            if let Err(e) = service::install(&argv[2..]) {
                eprintln!("install failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("uninstall") => {
            if let Err(e) = service::uninstall() {
                eprintln!("uninstall failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("update") => {
            if let Err(e) = service::update() {
                eprintln!("update failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("-h") | Some("--help") | Some("help") => {
            print_usage();
            return;
        }
        _ => {}
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = config::load();
    let auth = Auth::load(&cfg.config_dir, cfg.password.clone(), cfg.api_token.clone());
    tracing::info!(
        "cc-screen-hub: config={} client-auth {} per-agent-tokens={} ({})",
        cfg.config_dir.display(),
        if auth.enabled() { "ENABLED" } else { "disabled" },
        cfg.agent_tokens.len(),
        if cfg.agent_tokens.is_empty() { "open uplink — tailnet/dev only" } else { "uplink gated" },
    );
    if auth.weak_password() {
        tracing::warn!(
            "cc-screen-hub: CCWEB_PASSWORD is short (<12 chars) — weak against online \
             guessing if the hub is fronted to the internet; prefer a long passphrase"
        );
    }

    // Fail closed before binding: a routable bind with client auth disabled, or
    // with an OPEN uplink (no per-agent tokens), is refused unless the matching
    // loud override is set. The hub concentrates access to every agent's PTYs and
    // files, so an open default here is fleet-wide RCE.
    if let Err(msg) = cc_screen_auth::require_safe_bind(
        &cfg.addr,
        auth.enabled(),
        cfg.allow_unauthenticated_remote,
        "CCWEB_PASSWORD and/or CCWEB_API_TOKEN",
        "CCWEB_ALLOW_UNAUTHENTICATED_REMOTE",
    ) {
        eprintln!("cc-screen-hub: {msg}");
        std::process::exit(1);
    }
    if let Err(msg) = cc_screen_auth::require_gated_uplink(
        &cfg.addr,
        !cfg.agent_tokens.is_empty(),
        cfg.allow_open_uplink,
    ) {
        eprintln!("cc-screen-hub: {msg}");
        std::process::exit(1);
    }

    let hub = HubState {
        registry: Registry::new(),
        agent_tokens: Arc::new(cfg.agent_tokens),
        allow_open_uplink: cfg.allow_open_uplink,
        client_auth: auth,
        origin: cc_screen_auth::OriginPolicy::new(&cfg.addr, cfg.allowed_origins.as_deref()),
        login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
        push: Arc::new(cc_screen_push::Push::new(&cfg.config_dir)),
        config_dir: cfg.config_dir,
        bulk: Default::default(),
    };

    let app = build_router(hub);

    let listener = tokio::net::TcpListener::bind(&cfg.addr)
        .await
        .unwrap_or_else(|e| panic!("bind {}: {e}", cfg.addr));
    tracing::info!("cc-screen-hub: listening on http://{}", cfg.addr);
    axum::serve(listener, app).await.unwrap();
}
