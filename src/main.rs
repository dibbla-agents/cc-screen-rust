// cc-screen-rust — a web-only, tmux-free engine for driving AI coding CLIs
// (claude/kimi/gemini/codex) from a phone. The backend owns each session's PTY
// directly (no tmux), maintains a real terminal-emulator screen + scrollback
// model (see render.rs) so (re)attach is served as a clean, size-agnostic
// repaint, and serves the existing React PWA embedded in the binary. See
// PLAN.md for the design and milestones.

mod attach;
mod auth;
mod bulk;
mod clip;
mod config;
mod confine;
mod engine;
mod fileops;
mod files;
mod handlers;
mod manifest;
mod ops;
mod push;
mod render;
mod service;
mod tools;
mod uplink;
mod upload;
mod watch;

use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};

const UPLOAD_MAX: usize = 500 << 20; // 500 MiB
const CLIP_MAX: usize = 25 << 20; // 25 MiB

/// Runtime usage (when you run the binary directly). Service setup is a separate
/// subcommand — see `cc-screen-rust install --help`.
fn print_usage() {
    println!(
        r#"cc-screen-rust — the per-machine agent: drives AI coding CLIs (claude, codex,
gemini, kimi) as long-lived terminal sessions you attach to from a phone/browser
or the `ccs` TUI. Tailnet-only by design.

USAGE
  cc-screen-rust [--addr HOST:PORT] [--no-restore] [slave flags]
  cc-screen-rust install [--help]    set it up as an auto-starting service (usual way)
  cc-screen-rust update              fetch the latest release + restart the service
  cc-screen-rust uninstall           remove that service
  cc-screen-rust install-shim        (re)install the clipboard image-paste shim only

RUN-DIRECTLY FLAGS (for one-off / foreground runs)
  --addr HOST:PORT    bind address (default 127.0.0.1:8839; env CCWEB_ADDR)
  --no-restore        don't auto-resume recorded sessions at startup

SLAVE MODE (also register with a central hub; env in parens)
  --hub URL           hub to dial out to and register with     (CCWEB_HUB_URL)
  --hub-token TOK     this machine's per-agent uplink token    (CCWEB_HUB_TOKEN)
  --machine-id NAME   name shown in the hub's list (default hostname, CCWEB_MACHINE_ID)
  --hub-only          bind no local port; reachable only via the hub (CCWEB_HUB_ONLY)

AUTH (opt-in): set CCWEB_PASSWORD and/or CCWEB_API_TOKEN (see `install --help`).

Most people use `cc-screen-rust install` (which writes ~/.config/cc-screen-rust/
web.env and runs it in the background). To aggregate many machines under one
address, run a hub (`cc-screen-hub install`) and point agents at it with --hub."#
    );
}

#[tokio::main]
async fn main() {
    // `install` / `uninstall` wire up (or tear down) this binary's own service
    // (systemd on Linux, launchd on macOS) and exit — no server, no tracing.
    // `--help` prints runtime usage; otherwise we start serving.
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("install") => {
            if let Err(e) = service::install(&argv[2..]) {
                eprintln!("install failed: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some("install-shim") => {
            // Just (re)install the clipboard image-paste shim into ~/.local/bin,
            // without touching the service. Used by install.sh's --no-service path
            // and handy for a manual refresh after an update.
            if let Err(e) = service::install_shim() {
                eprintln!("install-shim failed: {e}");
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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = config::load();
    let tools = tools::load_tools(cfg.tools_path.clone());
    let prefixes: Vec<String> = tools.iter().map(|t| t.prefix.clone()).collect();
    tracing::info!(
        "cc-screen-rust: tools={:?} config={}",
        prefixes,
        cfg.config_dir.display()
    );

    let auth = auth::Auth::load(&cfg.config_dir, cfg.password.clone(), cfg.api_token.clone());
    let auth_enabled = auth.enabled();
    tracing::info!(
        "cc-screen-rust: auth {}",
        if auth_enabled { "ENABLED (password/token required)" } else { "disabled (tailnet-only, no gate)" }
    );
    if auth.weak_password() {
        tracing::warn!(
            "cc-screen-rust: CCWEB_PASSWORD is short (<12 chars) — weak against online \
             guessing if this is reachable off-tailnet; prefer a long passphrase or rely \
             on the API token"
        );
    }

    let origin = auth::OriginPolicy::new(&cfg.addr, cfg.allowed_origins.as_deref());
    let state = engine::AppState::new(
        tools,
        cfg.env_path.clone(),
        cfg.clip_url.clone(),
        cfg.config_dir.clone(),
        cfg.home.clone(),
        cfg.machine_id.clone(),
        auth,
        origin,
    );

    // Auto-restore recorded sessions at startup (resume-only model: a redeploy /
    // reboot ended the agents, so bring them back resuming each conversation).
    if cfg.no_restore {
        tracing::info!("cc-screen-rust: --no-restore set; not resuming sessions");
    } else {
        let (restored, failed) = state.restore_all();
        if !restored.is_empty() || !failed.is_empty() {
            tracing::info!("cc-screen-rust: restored {:?} failed {:?}", restored, failed);
        }
    }

    // Watch for agents finishing their turn (busy→waiting) and buzz subscribed
    // phones via Web Push. Cheap idle poll; no-op until a device subscribes.
    tokio::spawn(push::finish_watcher(state.clone()));

    // If pointed at a hub, also dial out and register. Dual-mode: the local bind
    // below still serves direct clients unless --hub-only.
    if let Some(hub) = cfg.hub_url.clone() {
        tokio::spawn(uplink::run(
            state.clone(),
            hub,
            cfg.hub_token.clone(),
            cfg.machine_id.clone(),
        ));
    }

    let app = Router::new()
        // terminal core
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/sessions/restorable", get(handlers::restorable))
        .route("/api/sessions/restore", post(handlers::restore))
        .route("/api/ws", get(handlers::ws))
        .route("/api/key", post(handlers::key))
        .route("/api/paste", post(handlers::paste))
        .route("/api/clear-history", post(handlers::clear_history))
        .route("/api/tools", get(handlers::tools))
        .route("/api/session", post(handlers::create_session))
        .route("/api/session/delete", post(handlers::delete_session))
        .route("/api/session/root", get(handlers::session_root))
        .route(
            "/api/favorites",
            get(handlers::get_favorites).put(handlers::put_favorites),
        )
        // web push ("agent finished" phone notifications)
        .route("/api/push/key", get(handlers::push_key))
        .route("/api/push/subscribe", post(handlers::push_subscribe))
        .route("/api/push/unsubscribe", post(handlers::push_unsubscribe))
        .route("/api/push/test", post(handlers::push_test))
        // files / editor
        .route("/api/dirs", get(files::dirs))
        .route("/api/files", get(files::files))
        .route("/api/download", get(files::download))
        .route("/api/file/read", get(files::file_read))
        .route("/api/file/write", post(files::file_write))
        .route("/api/file/delete", post(files::file_delete))
        .route("/api/mkdir", post(files::mkdir))
        .route("/api/rmdir", post(files::rmdir))
        .route("/api/rename", post(files::rename))
        // real-time filesystem watch (editor tree + open file)
        .route("/api/watch", get(watch::watch_ws))
        // upload (raised body limit)
        .route("/api/upload/check", post(upload::upload_check))
        .route(
            "/api/upload",
            post(upload::upload).layer(DefaultBodyLimit::max(UPLOAD_MAX)),
        )
        // clipboard image relay (raised body limit)
        .route(
            "/api/clip",
            post(clip::clip_put).layer(DefaultBodyLimit::max(CLIP_MAX)),
        )
        .route("/api/clip/targets", get(clip::clip_targets))
        .route("/api/clip/image.png", get(clip::clip_image))
        // auth (opt-in; these three are exempt inside the middleware)
        .route("/api/login", post(handlers::login))
        .route("/api/auth", get(handlers::auth_status))
        .route("/api/logout", post(handlers::logout))
        .fallback(handlers::static_handler)
        // Gate everything above; a no-op when no password/token is configured.
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::require_auth))
        .with_state(state);

    // --hub-only: bind NO inbound socket — the YOLO box is reachable only through
    // the hub (the uplink task spawned above does the work). Park until killed.
    if cfg.hub_only && cfg.hub_url.is_some() {
        tracing::info!(
            "cc-screen-rust: --hub-only; not binding a local port (reachable only via the hub)"
        );
        let _ = app; // built but unused in this mode
        std::future::pending::<()>().await;
        return;
    }

    // Fail closed: refuse a routable bind with auth disabled — a YOLO control
    // plane open to the tailnet/LAN is RCE for any peer. Loopback dev is fine;
    // CCWEB_ALLOW_UNAUTHENTICATED_REMOTE=1 is the loud override.
    if let Err(msg) = auth::require_safe_bind(
        &cfg.addr,
        auth_enabled,
        cfg.allow_unauthenticated_remote,
        "CCWEB_PASSWORD and/or CCWEB_API_TOKEN",
        "CCWEB_ALLOW_UNAUTHENTICATED_REMOTE",
    ) {
        eprintln!("cc-screen-rust: {msg}");
        std::process::exit(1);
    }

    let listener = tokio::net::TcpListener::bind(&cfg.addr)
        .await
        .unwrap_or_else(|e| panic!("bind {}: {e}", cfg.addr));
    tracing::info!("cc-screen-rust: listening on http://{}", cfg.addr);
    axum::serve(listener, app).await.unwrap();
}
