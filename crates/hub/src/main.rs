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
  cc-screen-hub user <add|agent|plan|delete> …
                                       manage multi-tenant accounts (needs
                                       CCHUB_DATABASE_URL; `user --help` for details)
  cc-screen-hub org <create|seats|info> …
                                       manage teams/orgs (proposal 0063; needs
                                       CCHUB_DATABASE_URL; `org --help` for details)

RUN-DIRECTLY FLAGS
  --addr HOST:PORT    bind address (default 127.0.0.1:8840; env CCWEB_ADDR)

CONFIG (env / ~/.config/cc-screen-hub/web.env)
  CCWEB_PASSWORD / CCWEB_API_TOKEN   client auth gate (the browser/TUI login)
  CCHUB_AGENT_TOKENS                 per-agent uplink tokens, "machine:token,m2:tok2".
                                     Empty = OPEN uplink (any agent may register); the
                                     hub refuses to start in that case (even on loopback —
                                     it may be tunnel-fronted) unless CCHUB_ALLOW_OPEN_UPLINK=1.
                                     Set tokens to require known agents. (Multi-tenant mode
                                     gates every uplink on per-agent DB tokens instead.)
  CCWEB_CONFIG_DIR                   override the state dir (default ~/.config/cc-screen-hub)
                                     so a second hub (e.g. a test instance on another
                                     port) runs with fully isolated state.

MULTI-TENANT (SaaS) MODE
  Set CCHUB_DATABASE_URL (e.g. sqlite:///path/hub.db) and the hub becomes a
  multi-account service: public signup + Google sign-in, per-user machine
  enrollment via <hub>/activate, per-plan machine/session caps
  (`user plan <email> free|pro|unlimited`). Unset = classic single-tenant hub.
  CCHUB_PUBLIC_URL       canonical public origin (installer + OAuth + /activate URLs)
  GOOGLE_OAUTH_CLIENT_ID / _SECRET   enable "Sign in with Google"
  CCHUB_OAUTH_ONLY=1     disable password signup/login (Google only)

  Stripe self-serve billing (proposal 0058) — OFF unless these are set. With no
  STRIPE_* env the /api/billing/* routes 404, /api/me reports billing:false, and
  no reconcile task spawns (a self-hosted hub is exactly today's hub):
  STRIPE_SECRET_KEY          restricted key (Checkout+Portal write, Subscriptions read)
  STRIPE_WEBHOOK_SECRET      signing secret for /api/billing/webhook (whsec_…)
  STRIPE_PRICE_PRO_MONTHLY / STRIPE_PRICE_PRO_ANNUAL   the two Pro price ids
  STRIPE_PRICE_PRO_FOUNDER   optional $5/mo founder price (beta cohort, until…)
  STRIPE_PRICE_TEAM_MONTHLY / STRIPE_PRICE_TEAM_ANNUAL   optional Team per-seat
                             price ids ($16/seat, $160/seat/yr; proposal 0064) —
                             unset ⇒ Pro-only billing, Team checkout 400s
  STRIPE_PRICE_TEAM_FOUNDER  optional $8/seat founder price (beta-cohort owners)
  STRIPE_PORTAL_CONFIG_TEAM  optional bpc_… portal configuration for TEAM (org)
                             portal sessions — quantity adjustable (min 3) on
                             the Team prices only; unset = account default
  STRIPE_FOUNDER_DEADLINE    optional unix-seconds cutoff for the founder offer
  STRIPE_MANAGED_PAYMENTS=1  opt-in: Stripe Managed Payments (merchant of record);
                             requires MoR activated on the Stripe account

SETUP
  1. On the hub box:   cc-screen-hub install --password PW --agents 'laptop:T1,server:T2'
  2. On each machine:  cc-screen-rust install --hub https://HUB:8840 --hub-token T1 --machine-id laptop
  3. Open the hub URL in a browser, or:  ccs --server https://HUB:8840 --token <client-token>

Off-tailnet: front the hub with a TLS reverse proxy and always set CCHUB_AGENT_TOKENS."#
    );
}

/// `cc-screen-hub user add <email> <password>` / `user agent <email> <machine>` —
/// operator CLI for multi-tenant accounts, alongside the public signup + device
/// enrollment the hub serves ([0001] §10.2–§10.4): hand-provision an account,
/// mint an uplink token, assign a plan, or delete a user. Reads CCHUB_DATABASE_URL.
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

/// `cc-screen-hub org create <name> <owner-email>` / `org seats <name-or-id> <n>`
/// / `org info <name-or-id>` — the founder's pre-billing org tooling (proposal
/// 0063 B4): what makes the org model shippable and testable BEFORE 0064's
/// self-serve billing, exactly the manual-plans precedent.
#[cfg(feature = "multi-tenant")]
async fn org_admin(args: &[String]) -> anyhow::Result<()> {
    use cc_screen_hub::db::{SqliteStore, Store};
    let usage = "usage: cc-screen-hub org create <name> <owner-email>\n       \
                 cc-screen-hub org seats <name-or-id> <n>   (hand-set the seat count)\n       \
                 cc-screen-hub org info <name-or-id>\n\
                 (database via CCHUB_DATABASE_URL, e.g. sqlite:///path/hub.db)";
    let url = std::env::var("CCHUB_DATABASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("set CCHUB_DATABASE_URL\n{usage}"))?;
    let store = SqliteStore::connect(&url).await?;
    match args.first().map(String::as_str) {
        Some("create") => {
            let (Some(name), Some(email)) = (args.get(1), args.get(2)) else {
                anyhow::bail!("missing name/owner-email\n{usage}");
            };
            let owner = store
                .user_id_by_email(email)
                .await
                .ok_or_else(|| anyhow::anyhow!("no such user: {email}"))?;
            let id = store.org_create(&owner, name).await?;
            println!("created team '{name}' (id {id}) — owner {email}, 0 seats");
            println!("activate it with:  cc-screen-hub org seats {id} <n>");
        }
        Some("seats") => {
            let (Some(key), Some(n)) = (args.get(1), args.get(2)) else {
                anyhow::bail!("missing name-or-id/seats\n{usage}");
            };
            let seats: i64 = n.parse().map_err(|_| anyhow::anyhow!("seats must be a number\n{usage}"))?;
            let org = store
                .org_by_name_or_id(key)
                .await
                .ok_or_else(|| anyhow::anyhow!("no such org (or ambiguous name — use the id): {key}"))?;
            store.org_set_seats(&org.id, seats).await?;
            store
                .audit_append(&org.id, None, "org.seats_changed", None, Some(&format!("{{\"seats\":{seats},\"via\":\"cli\"}}")))
                .await;
            println!("set '{}' → {seats} seats", org.name);
        }
        Some("info") => {
            let Some(key) = args.get(1) else { anyhow::bail!("missing name-or-id\n{usage}") };
            let org = store
                .org_by_name_or_id(key)
                .await
                .ok_or_else(|| anyhow::anyhow!("no such org (or ambiguous name — use the id): {key}"))?;
            let members = store.org_members(&org.id).await;
            println!("team '{}'  (id {})", org.name, org.id);
            println!("  seats: {}   members: {}   status: {}", org.seat_count, members.len(), org.plan_status.as_deref().unwrap_or("—"));
            for m in members {
                println!("  {:8} {}", m.role, m.email);
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
        // Operator CLI for multi-tenant accounts (add/agent/plan/delete) — the
        // admin companion to the public signup + device flow the hub serves.
        // DB via CCHUB_DATABASE_URL. Only in a multi-tenant build.
        #[cfg(feature = "multi-tenant")]
        Some("user") => {
            if let Err(e) = user_admin(&argv[2..]).await {
                eprintln!("user: {e}");
                std::process::exit(1);
            }
            return;
        }
        // Org/team tooling (proposal 0063 B4) — same store-backed shape.
        #[cfg(feature = "multi-tenant")]
        Some("org") => {
            if let Err(e) = org_admin(&argv[2..]).await {
                eprintln!("org: {e}");
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

    // Transactional mail (proposal 0073). Resolved once, here, so the whole
    // process shares one transport; off unless CCHUB_SMTP_URL (or the
    // CCHUB_MAIL_DIR capture transport) *and* CCHUB_PUBLIC_URL are both set.
    #[cfg(feature = "multi-tenant")]
    let mailer = cc_screen_hub::mailer::Mailer::from_env();
    #[cfg(feature = "multi-tenant")]
    tracing::info!(
        "cc-screen-hub: invite email {}",
        if mailer.active() {
            format!("ENABLED (links built on {})", mailer.public_url())
        } else {
            "disabled (no CCHUB_SMTP_URL/CCHUB_MAIL_DIR + CCHUB_PUBLIC_URL) — the copyable invite link is the channel".to_string()
        },
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
        #[cfg(feature = "multi-tenant")]
        mailer: Arc::new(mailer),
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
                // Reap expired pending invites + long-dead terminal rows (0040 §7).
                store.share_sweep().await;
                // Org invites ride the same timer (proposal 0063 B2).
                store.org_invite_sweep().await;
                // Fail-stamp send attempts the process lost (proposal 0073 B2):
                // the hub has no graceful shutdown, so a restart drops in-flight
                // spawns and a `sending` row would otherwise read as
                // "never attempted" forever.
                store.invite_delivery_sweep().await;
            }
        });
    }

    // Nightly team-share reconcile (proposal 0065 A3): the membership hooks are
    // the primary mechanism; this is the invariant-restorer for any missed hook.
    // Independent of Stripe — it runs on every multi-tenant hub.
    #[cfg(feature = "multi-tenant")]
    if let Some(store) = hub.store() {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                store.team_shares_reconcile().await;
            }
        });
    }

    // Nightly billing reconcile (proposal 0058 B5): the webhook is at-least-once,
    // so this restores the invariant every 24h. Only when Stripe is configured —
    // an unconfigured hub spawns no task at all (graceful absence).
    #[cfg(feature = "multi-tenant")]
    if cc_screen_hub::billing::is_configured() {
        if let Some(store) = hub.store() {
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(24 * 60 * 60));
                tick.tick().await; // consume the immediate first tick
                loop {
                    tick.tick().await;
                    cc_screen_hub::billing::reconcile(store.clone()).await;
                }
            });
        }
    }

    let app = build_router(hub);

    let listener = tokio::net::TcpListener::bind(&cfg.addr)
        .await
        .unwrap_or_else(|e| panic!("bind {}: {e}", cfg.addr));
    tracing::info!("cc-screen-hub: listening on http://{}", cfg.addr);
    axum::serve(listener, app).await.unwrap();
}
