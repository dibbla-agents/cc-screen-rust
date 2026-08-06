//! The cc-screen-hub library: the registry, the agent uplink server, the client
//! bridges, and the router assembly. The `cc-screen-hub` binary (`main.rs`) is a
//! thin wrapper; exposing this as a lib lets the integration tests mount the
//! identical router + relay on an ephemeral port.

pub mod assets;
pub mod bulk;
/// `GET /install.sh` — the hub-served machine installer (proposal 0001 Phase 3).
pub mod install;
pub mod client_ws;
pub mod config;
/// Multi-tenant store (proposal 0001) — compiled only with `--features
/// multi-tenant`; absent from the single-tenant build.
#[cfg(feature = "multi-tenant")]
pub mod db;
/// Google "Sign in with Google" backend (proposal 0001 §3.3) — multi-tenant only.
#[cfg(feature = "multi-tenant")]
pub mod oauth;
/// RFC-8628 headless device-enrollment endpoints (proposal 0001 §6–8) — multi-tenant only.
#[cfg(feature = "multi-tenant")]
pub mod device;
/// Account + dashboard endpoints (signup, agent list/unlink/rotate) — multi-tenant only.
#[cfg(feature = "multi-tenant")]
pub mod account;
/// Share-invite lifecycle endpoints (proposal 0040) — multi-tenant only.
#[cfg(feature = "multi-tenant")]
pub mod share;
/// Stripe self-serve billing (proposal 0058 Part B) — multi-tenant only, and its
/// routes only register when `billing::is_configured()` (STRIPE_* env present).
#[cfg(feature = "multi-tenant")]
pub mod billing;
pub mod handlers;
pub mod registry;
pub mod service;
pub mod state;
pub mod summarizer;
pub mod uplink_server;
pub mod watch_ws;

// In-process hub + fake-agent doubles for e2e tests (proposal 0059 B1). Behind a
// feature so a normal hub build never compiles them; consumed by the hub's own
// `tests/e2e.rs` and, as a dev-dependency, by the `ccs` TUI e2e harness.
#[cfg(feature = "test-support")]
pub mod test_support;

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
    let router = Router::new()
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
        // Coding-assistant updates (0049) — owner-only, and only for an agent that
        // advertised the capability (else 501, never a 504 from an old agent).
        .route(
            "/api/assistants/update",
            get(handlers::update_status).post(handlers::update_assistants),
        )
        // …and what installing the missing ones would do (0050), same gate.
        .route("/api/assistants/plan", get(handlers::install_plan))
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
        .route("/api/logout", post(handlers::logout));

    // Google "Sign in with Google" (proposal 0001 §3.3) — multi-tenant only, so
    // the routes only exist in that build. Exempt from the auth gate (they ARE the
    // login) via the `/api/auth/google/` prefix in `require_client_auth`.
    #[cfg(feature = "multi-tenant")]
    let router = router
        .route("/api/auth/google/start", get(oauth::google_start))
        .route("/api/auth/google/callback", get(oauth::google_callback))
        // Device enrollment (§8): /code + /token are host-facing (unauthenticated —
        // the device_code is the bearer); /approve is cookie-authed.
        .route("/api/device/code", post(device::code))
        .route("/api/device/token", post(device::token))
        .route("/api/device/validate", post(device::validate))
        .route("/api/device/approve", post(device::approve))
        // Terminal-client sign-in (proposal 0060 B2): the same RFC-8628 flow on
        // distinct paths (so a pre-0060 hub 404s clean). /approve is shared.
        .route("/api/device/client/code", post(device::client_code))
        .route("/api/device/client/token", post(device::client_token))
        // Terminal-client token management (0060 B4): the account page's list +
        // revoke (cookie-authed) and `ccs logout`'s self-revoke (Bearer-authed).
        .route("/api/client-tokens", get(account::client_tokens_list))
        .route("/api/client-tokens/delete", post(account::client_tokens_delete))
        .route("/api/client-tokens/revoke-self", post(account::client_tokens_revoke_self))
        // Account + dashboard. /signup is unauthenticated (it mints the session);
        // the agent-management routes are cookie-authed.
        .route("/api/signup", post(account::signup))
        .route("/api/agents", get(account::list))
        .route("/api/agents/unlink", post(account::unlink))
        .route("/api/agents/rotate", post(account::rotate))
        // Sharing (proposal 0040): the invite lifecycle that produces the 0039
        // grant. Create/re-invite, the grantee's inbox + the inviter's outbox, and
        // accept/decline/revoke (owner/grantee-scoped, idempotent).
        .route("/api/shares", post(share::create))
        .route("/api/shares/inbox", get(share::inbox))
        .route("/api/shares/outbox", get(share::outbox))
        .route("/api/shares/received", get(share::received))
        .route("/api/shares/received/:id/leave", post(share::leave))
        .route("/api/shares/:id/accept", post(share::accept))
        .route("/api/shares/:id/decline", post(share::decline))
        .route("/api/shares/:id/revoke", post(share::revoke))
        // Email-invite landing read (proposal 0056 C4): unauthenticated (the
        // token is the capability; exempted in require_client_auth), throttled.
        .route("/api/invite/:token", get(share::invite_info))
        // The `ccs` terminal-client install one-liners (proposal 0060 D3): the
        // /install.sh template idea, ending in `ccs activate --server <hub>`.
        // Multi-tenant only — `activate` is the multi-tenant sign-in, and the
        // single-tenant build stays byte-for-byte route-free of 0060.
        .route("/ccs.sh", get(install::ccs_sh))
        .route("/ccs.ps1", get(install::ccs_ps1));

    // Stripe billing (proposal 0058 Part B): register only when configured, so an
    // unconfigured hub 404s these paths (the handlers also re-check config as
    // defense in depth). The webhook is cookie-gate-exempt (its Stripe-Signature
    // HMAC is the auth) — see `handlers::require_client_auth`.
    #[cfg(feature = "multi-tenant")]
    let router = if billing::is_configured() {
        router
            .route("/api/billing/checkout", post(billing::checkout))
            .route("/api/billing/portal", post(billing::portal))
            .route("/api/billing/webhook", post(billing::webhook))
    } else {
        router
    };

    // The embedded PWA (exempt from auth — it's the app shell).
    router
        // The machine installer one-liners (public): POSIX `curl <hub>/install.sh | sh`
        // and the Windows `irm <hub>/install.ps1 | iex` twin (proposal 0045).
        .route("/install.sh", get(install::install_sh))
        .route("/install.ps1", get(install::install_ps1))
        .fallback(assets::static_handler)
        .layer(axum::middleware::from_fn_with_state(hub.clone(), handlers::require_client_auth))
        .with_state(hub)
}
