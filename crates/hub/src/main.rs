// cc-screen-hub — the aggregator binary. A thin wrapper over the crate library
// (see lib.rs): parse args, wire up HubState, build the router, serve. Agents dial
// in over `/agent/ws` and register; clients speak the same wire contract they'd
// speak to a single agent, and the hub routes each request to the owning machine.
// The hub owns NO PTY and NO filesystem — it is a registry + client-auth gate +
// transparent byte relay.

use std::sync::Arc;

use cc_screen_auth::Auth;
use cc_screen_hub::{
    build_router, config, registry::Registry, service,
    state::{HubState, Tenancy},
    summarizer::Summarizer,
};

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

/// `cc-screen-hub user add <email> <password>` / `user agent <email> <machine>` —
/// hand-provision a multi-tenant account or mint an agent uplink token for it
/// (Phase 1 has no public signup / device flow yet). Reads CCHUB_DATABASE_URL.
#[cfg(feature = "multi-tenant")]
async fn user_admin(args: &[String]) -> anyhow::Result<()> {
    use cc_screen_hub::db::{SqliteStore, Store};
    let usage = "usage: cc-screen-hub user add <email> <password>\n       \
                 cc-screen-hub user agent <email> <machine_id>   (mints an uplink token)\n       \
                 cc-screen-hub user plan <email> <plan>          (free | pro | unlimited | …)\n       \
                 cc-screen-hub user delete <email>               (removes the user + their agents)\n\
                 (database via CCHUB_DATABASE_URL, e.g. sqlite:///path/hub.db)";
    let url = std::env::var("CCHUB_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("set CCHUB_DATABASE_URL\n{usage}"))?;
    let store = SqliteStore::connect(&url).await?;
    match args.first().map(String::as_str) {
        Some("add") => {
            let (email, password) = (args.get(1), args.get(2));
            let (Some(email), Some(password)) = (email, password) else {
                anyhow::bail!("missing email/password\n{usage}");
            };
            let id = store.create_user(email, password).await?;
            println!("created user {email}  (id {id})");
        }
        Some("agent") => {
            let (Some(email), Some(machine)) = (args.get(1), args.get(2)) else {
                anyhow::bail!("missing email/machine_id\n{usage}");
            };
            let user_id = store
                .user_id_by_email(email)
                .await
                .ok_or_else(|| anyhow::anyhow!("no such user: {email}"))?;
            let (token, agent_id) = store.upsert_agent(&user_id, machine).await?;
            println!("agent '{machine}' bound to {email}  (id {agent_id})");
            println!("uplink token (shown once — store it now):\n  {token}");
        }
        Some("plan") => {
            let (Some(email), Some(plan)) = (args.get(1), args.get(2)) else {
                anyhow::bail!("missing email/plan\n{usage}");
            };
            store.set_plan(email, plan).await?;
            println!("set {email} → plan '{plan}'");
        }
        Some("delete") => {
            let Some(email) = args.get(1) else { anyhow::bail!("missing email\n{usage}") };
            if store.delete_user(email).await {
                println!("deleted {email} (and any agents)");
            } else {
                println!("no such user: {email}");
            }
        }
        _ => anyhow::bail!("{usage}"),
    }
    Ok(())
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
        // Hand-provision a multi-tenant account (proposal 0001 Phase 1 — no public
        // signup yet). DB via CCHUB_DATABASE_URL. Only in a multi-tenant build.
        #[cfg(feature = "multi-tenant")]
        Some("user") => {
            if let Err(e) = user_admin(&argv[2..]).await {
                eprintln!("user: {e}");
                std::process::exit(1);
            }
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

    // Tenancy (proposal 0001): multi-tenant only in a `--features multi-tenant`
    // build AND with CCHUB_DATABASE_URL set; otherwise single-tenant — today's
    // behavior. A default build ignores the URL entirely.
    let tenancy: Tenancy;
    let multi_tenant: bool;
    #[cfg(feature = "multi-tenant")]
    {
        match cfg.database_url.as_deref() {
            Some(url) => match cc_screen_hub::db::SqliteStore::connect(url).await {
                Ok(store) => {
                    tracing::info!("cc-screen-hub: MULTI-TENANT mode (db={url})");
                    tenancy = Tenancy::Multi(std::sync::Arc::new(store));
                    multi_tenant = true;
                }
                Err(e) => {
                    eprintln!("cc-screen-hub: failed to open database {url}: {e}");
                    std::process::exit(1);
                }
            },
            None => {
                tenancy = Tenancy::Single;
                multi_tenant = false;
            }
        }
    }
    #[cfg(not(feature = "multi-tenant"))]
    {
        if cfg.database_url.is_some() {
            tracing::warn!(
                "cc-screen-hub: CCHUB_DATABASE_URL is set but this binary was built without \
                 the `multi-tenant` feature — running single-tenant, ignoring it"
            );
        }
        tenancy = Tenancy::Single;
        multi_tenant = false;
    }
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
        // Multi-tenant gates every uplink on a per-agent DB token, so it counts as
        // gated even with an empty static CCHUB_AGENT_TOKENS map.
        !cfg.agent_tokens.is_empty() || multi_tenant,
        cfg.allow_open_uplink,
    ) {
        eprintln!("cc-screen-hub: {msg}");
        std::process::exit(1);
    }

    let summarizer = Summarizer::new(
        cfg.summary_enabled,
        cfg.anthropic_api_key,
        cfg.summary_model.clone(),
        cfg.summary_budget_usd,
        cfg.summary_user_budget_usd,
    );
    tracing::info!(
        "cc-screen-hub: session summaries {} (model={}, budget={})",
        if summarizer.active() { "ENABLED" } else { "disabled (no key or CCHUB_SUMMARY=off)" },
        cfg.summary_model,
        cfg.summary_budget_usd.map(|b| format!("${b:.2}")).unwrap_or_else(|| "uncapped".into()),
    );

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
        summary: Arc::new(summarizer),
        tenancy,
    };

    // Reap expired device enrollments on a timer (proposal 0001 §8.4). Multi-tenant
    // only; cheap indexed DELETE on a small table.
    #[cfg(feature = "multi-tenant")]
    if let Some(store) = hub.store() {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                store.device_sweep().await;
            }
        });
    }

    let app = build_router(hub);

    let listener = tokio::net::TcpListener::bind(&cfg.addr)
        .await
        .unwrap_or_else(|e| panic!("bind {}: {e}", cfg.addr));
    tracing::info!("cc-screen-hub: listening on http://{}", cfg.addr);
    axum::serve(listener, app).await.unwrap();
}
