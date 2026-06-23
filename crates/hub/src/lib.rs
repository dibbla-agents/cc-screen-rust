//! The cc-screen-hub library: the registry, the agent uplink server, the client
//! bridges, and the router assembly. The `cc-screen-hub` binary (`main.rs`) is a
//! thin wrapper; exposing this as a lib lets the integration tests mount the
//! identical router + relay on an ephemeral port.

pub mod assets;
pub mod bulk;
pub mod client_ws;
pub mod config;
/// Multi-tenant Postgres store (proposal 0001) — compiled only with `--features
/// multi-tenant`; absent from the single-tenant build.
#[cfg(feature = "multi-tenant")]
pub mod db;
pub mod handlers;
pub mod registry;
pub mod service;
pub mod state;
pub mod summarizer;
pub mod uplink_server;
pub mod watch_ws;

use axum::routing::{get, post};
use axum::Router;

use state::HubState;

/// Upload body ceiling on the hub relay — the agent allows 500 MiB; add headroom
/// for multipart framing so the hub never rejects a transfer the agent accepts.
const UPLOAD_MAX: usize = 520 << 20; // 520 MiB
/// Clipboard-image body ceiling (the agent allows 25 MiB).
const CLIP_MAX: usize = 32 << 20; // 32 MiB

/// Assemble the hub router. The agent uplink (`/agent/ws`) carries its own
/// per-agent token check and is exempt from client auth (it isn't under `/api/`).
/// Everything under `/api/` rides the client-auth middleware.
pub fn build_router(hub: HubState) -> Router {
    Router::new()
        // The agent uplink + the dedicated bulk dial-back.
        .route("/agent/ws", get(uplink_server::agent_ws))
        .route("/agent/bulk", get(bulk::agent_bulk))
        // Bulk file transfers (download / upload / clipboard image), relayed to
        // the owning agent's real handlers over the dedicated bulk WS. Cap the
        // body at a sane ceiling (matching the agent's own limits, +headroom)
        // rather than disabling the limit entirely — bound memory/disk abuse.
        .route("/api/download", get(bulk::proxy))
        .route("/api/upload", post(bulk::proxy).layer(axum::extract::DefaultBodyLimit::max(UPLOAD_MAX)))
        .route("/api/upload/check", post(bulk::proxy))
        .route("/api/clip", post(bulk::proxy).layer(axum::extract::DefaultBodyLimit::max(CLIP_MAX)))
        .route("/api/clip/targets", get(bulk::proxy))
        .route("/api/clip/image.png", get(bulk::proxy))
        // Client-facing aggregation + auth.
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/machines", get(handlers::machines))
        .route("/api/tools", get(handlers::tools))
        // Terminal + filesystem-watch bridges.
        .route("/api/ws", get(client_ws::ws))
        .route("/api/watch", get(watch_ws::ws))
        // Session lifecycle + control, routed to the owning agent (?machine=).
        .route("/api/session", post(handlers::create))
        .route("/api/session/delete", post(handlers::delete))
        .route("/api/session/color", post(handlers::set_color))
        .route("/api/session/label", post(handlers::set_label))
        .route("/api/session/root", get(handlers::session_root))
        .route("/api/sessions/restorable", get(handlers::restorable))
        .route("/api/sessions/restore", post(handlers::restore))
        .route("/api/key", post(handlers::key))
        .route("/api/paste", post(handlers::paste))
        .route("/api/clear-history", post(handlers::clear_history))
        // File browser / editor (small ops), routed to the owning agent.
        .route("/api/dirs", get(handlers::dirs))
        .route("/api/dirs/search", get(handlers::dirs_search))
        .route("/api/files/search", get(handlers::files_search))
        .route("/api/files", get(handlers::files))
        .route("/api/file/read", get(handlers::file_read))
        .route("/api/file/write", post(handlers::file_write))
        .route("/api/file/delete", post(handlers::file_delete))
        .route("/api/mkdir", post(handlers::mkdir))
        .route("/api/rmdir", post(handlers::rmdir))
        .route("/api/rename", post(handlers::rename))
        .route("/api/move", post(handlers::move_path))
        // Hub-local: favorites + Web Push (one of each for the whole fleet).
        .route("/api/favorites", get(handlers::get_favorites).put(handlers::put_favorites))
        .route("/api/push/key", get(handlers::push_key))
        .route("/api/push/subscribe", post(handlers::push_subscribe))
        .route("/api/push/unsubscribe", post(handlers::push_unsubscribe))
        .route("/api/push/test", post(handlers::push_test))
        // Auth (exempt inside the middleware).
        .route("/api/login", post(handlers::login))
        .route("/api/auth", get(handlers::auth_status))
        .route("/api/me", get(handlers::me))
        .route("/api/logout", post(handlers::logout))
        // The embedded PWA (exempt from auth — it's the app shell).
        .fallback(assets::static_handler)
        .layer(axum::middleware::from_fn_with_state(hub.clone(), handlers::require_client_auth))
        .with_state(hub)
}
