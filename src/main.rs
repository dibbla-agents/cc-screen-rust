// cc-screen-rust — a web-only, tmux-free engine for driving AI coding CLIs
// (claude/kimi/gemini/codex) from a phone. The backend owns each session's PTY
// directly (no tmux), maintains a real terminal-emulator screen + scrollback
// model (see render.rs) so (re)attach is served as a clean, size-agnostic
// repaint, and serves the existing React PWA embedded in the binary. See
// PLAN.md for the design and milestones.

mod clip;
mod config;
mod confine;
mod engine;
mod files;
mod handlers;
mod manifest;
mod render;
mod tools;
mod upload;
mod watch;

use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};

const UPLOAD_MAX: usize = 500 << 20; // 500 MiB
const CLIP_MAX: usize = 25 << 20; // 25 MiB

#[tokio::main]
async fn main() {
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

    let state = engine::AppState::new(tools, cfg.env_path.clone(), cfg.config_dir.clone(), cfg.home.clone());

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
        .fallback(handlers::static_handler)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&cfg.addr)
        .await
        .unwrap_or_else(|e| panic!("bind {}: {e}", cfg.addr));
    tracing::info!("cc-screen-rust: listening on http://{}", cfg.addr);
    axum::serve(listener, app).await.unwrap();
}
