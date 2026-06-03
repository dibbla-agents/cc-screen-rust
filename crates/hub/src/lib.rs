//! The cc-screen-hub library: the registry, the agent uplink server, the client
//! bridges, and the router assembly. The `cc-screen-hub` binary (`main.rs`) is a
//! thin wrapper; exposing this as a lib lets the integration tests mount the
//! identical router + relay on an ephemeral port.

pub mod assets;
pub mod client_ws;
pub mod config;
pub mod handlers;
pub mod registry;
pub mod service;
pub mod state;
pub mod uplink_server;
pub mod watch_ws;

use axum::routing::{get, post};
use axum::Router;

use state::HubState;

/// Assemble the hub router. The agent uplink (`/agent/ws`) carries its own
/// per-agent token check and is exempt from client auth (it isn't under `/api/`).
/// Everything under `/api/` rides the client-auth middleware.
pub fn build_router(hub: HubState) -> Router {
    Router::new()
        // The agent uplink.
        .route("/agent/ws", get(uplink_server::agent_ws))
        // Client-facing aggregation + auth.
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/machines", get(handlers::machines))
        // Terminal + filesystem-watch bridges.
        .route("/api/ws", get(client_ws::ws))
        .route("/api/watch", get(watch_ws::ws))
        // Session lifecycle + control, routed to the owning agent (?machine=).
        .route("/api/session", post(handlers::create))
        .route("/api/session/delete", post(handlers::delete))
        .route("/api/session/root", get(handlers::session_root))
        .route("/api/sessions/restorable", get(handlers::restorable))
        .route("/api/sessions/restore", post(handlers::restore))
        .route("/api/key", post(handlers::key))
        .route("/api/paste", post(handlers::paste))
        .route("/api/clear-history", post(handlers::clear_history))
        // File browser / editor (small ops), routed to the owning agent.
        .route("/api/dirs", get(handlers::dirs))
        .route("/api/files", get(handlers::files))
        .route("/api/file/read", get(handlers::file_read))
        .route("/api/file/write", post(handlers::file_write))
        .route("/api/file/delete", post(handlers::file_delete))
        .route("/api/mkdir", post(handlers::mkdir))
        .route("/api/rmdir", post(handlers::rmdir))
        .route("/api/rename", post(handlers::rename))
        // Hub-local: favorites + Web Push (one of each for the whole fleet).
        .route("/api/favorites", get(handlers::get_favorites).put(handlers::put_favorites))
        .route("/api/push/key", get(handlers::push_key))
        .route("/api/push/subscribe", post(handlers::push_subscribe))
        .route("/api/push/unsubscribe", post(handlers::push_unsubscribe))
        .route("/api/push/test", post(handlers::push_test))
        // Auth (exempt inside the middleware).
        .route("/api/login", post(handlers::login))
        .route("/api/auth", get(handlers::auth_status))
        .route("/api/logout", post(handlers::logout))
        // The embedded PWA (exempt from auth — it's the app shell).
        .fallback(assets::static_handler)
        .layer(axum::middleware::from_fn_with_state(hub.clone(), handlers::require_client_auth))
        .with_state(hub)
}
