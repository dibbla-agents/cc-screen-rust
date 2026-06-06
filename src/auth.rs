// Opt-in auth for the tailnet-only server. The credential logic — the `Auth`
// struct, the signed-session-cookie scheme, token matching, `is_https`,
// `generate_token` — lives in the shared `cc-screen-auth` crate so the hub reuses
// it byte-for-byte. This module re-exports it and keeps the one axum-coupled
// piece: the `require_auth` middleware, which reads the gate off the agent's
// `AppState`. See `crates/auth/src/lib.rs` for the threat model and design.

pub use cc_screen_auth::*;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::engine::AppState;

/// Axum middleware enforcing the gate. Exempts the auth endpoints and every
/// static asset (the app shell carries no secrets and must load so the login
/// screen can render); everything else under `/api/` requires auth. The auth gate
/// is a no-op when no password/token is configured — but the Origin/Host check
/// runs **regardless** of the gate, since it defends the default-unauthenticated
/// case against a cross-origin / DNS-rebinding browser page driving a YOLO box.
pub async fn require_auth(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    // Browser trust boundary: reject cross-origin / rebinding requests to the API
    // (incl. the WS upgrades and the exempt auth endpoints), even with auth off.
    // Static assets are exempt — the app shell must load to render login, and a
    // foreign page can't read them cross-origin anyway.
    if path.starts_with("/api/") && !app.inner.origin.check(req.headers()) {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }

    let auth = &app.inner.auth;
    if !auth.enabled() {
        return next.run(req).await;
    }
    let exempt = !path.starts_with("/api/")
        || matches!(path, "/api/login" | "/api/auth" | "/api/logout");
    // `req.headers()` is `&http::HeaderMap` — the same type the auth crate takes.
    if exempt || auth.is_authed(req.headers(), req.uri().query()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}
