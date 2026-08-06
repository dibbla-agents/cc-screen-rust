//! The RFC-8628 device-authorization HTTP endpoints (proposal 0001 §8), compiled
//! only under `multi-tenant`. `/code` and `/token` are unauthenticated by design —
//! possession of the high-entropy `device_code` IS the credential; tenancy is
//! decided in exactly one place, `/approve`, where the browser's session cookie
//! binds the pending enrollment to a user.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::db::DevicePoll;
use crate::state::HubState;

fn public_base() -> String {
    std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:8840".to_string())
}

#[derive(Deserialize)]
pub struct CodeReq {
    device_id: String,
    machine_id: String,
}

/// How many device codes one source may mint per rolling minute (proposal 0060
/// B5), across both flows. The per-code slow_down still governs polling; this
/// bounds the *mint* rate (the pending-row count is already bounded by the
/// 10-minute lazy expiry). Generous for a human retrying, tight for a script.
const DEVICE_CODES_PER_MIN: usize = 30;

/// Per-source sliding window for the `code` endpoints — the same shape as
/// signup's (`account::signup_rate_limited`), with its own bucket.
fn code_rate_limited(source: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static WINDOWS: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    let mut map = WINDOWS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    let now = Instant::now();
    let cutoff = now - Duration::from_secs(60);
    if map.len() >= 4096 {
        map.retain(|_, v| v.iter().any(|t| *t > cutoff));
    }
    let hits = map.entry(source.to_string()).or_default();
    hits.retain(|t| *t > cutoff);
    if hits.len() >= DEVICE_CODES_PER_MIN {
        return true;
    }
    hits.push(now);
    false
}

/// `POST /api/device/code` — the headless host requests a code (unauthenticated).
pub async fn code(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<CodeReq>) -> Response {
    if code_rate_limited(&cc_screen_auth::source_key(&headers)) {
        return (StatusCode::TOO_MANY_REQUESTS, "too many codes requested").into_response();
    }
    if req.device_id.trim().is_empty() || req.machine_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "device_id and machine_id required").into_response();
    }
    let Some(c) = hub.device_create(&req.device_id, &req.machine_id).await else {
        return (StatusCode::NOT_IMPLEMENTED, "device flow unavailable").into_response();
    };
    Json(json!({
        "device_code": c.device_code,
        "user_code": c.user_code_display,
        "verification_uri": format!("{}/activate", public_base()),
        "interval": c.interval,
        "expires_in": c.expires_in,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ClientCodeReq {
    /// Client-suggested display label ("erik@orchid"); shown on the approve page
    /// and the account page's "Terminal clients" list.
    #[serde(default)]
    label: String,
}

/// `POST /api/device/client/code` — a terminal client (`ccs activate`) requests a
/// sign-in code (unauthenticated; proposal 0060 B2). A **distinct path** from the
/// machine flow so a pre-0060 hub answers a clean 404 ("too old"), never silently
/// runs the machine enrollment.
pub async fn client_code(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<ClientCodeReq>) -> Response {
    if code_rate_limited(&cc_screen_auth::source_key(&headers)) {
        return (StatusCode::TOO_MANY_REQUESTS, "too many codes requested").into_response();
    }
    let label = req.label.trim();
    let label = if label.is_empty() { "terminal" } else { label };
    // Bound the label (it renders on the approve + account pages).
    let label: String = label.chars().take(64).collect();
    let Some(c) = hub.device_create_client(&label).await else {
        return (StatusCode::NOT_IMPLEMENTED, "device flow unavailable").into_response();
    };
    Json(json!({
        "device_code": c.device_code,
        "user_code": c.user_code_display,
        "verification_uri": format!("{}/activate", public_base()),
        "interval": c.interval,
        "expires_in": c.expires_in,
    }))
    .into_response()
}

/// `POST /api/device/client/token` — the terminal polls (unauthenticated;
/// proposal 0060 B2). RFC-8628 error codes; the minted **client token** (plus the
/// account email, so the terminal echoes who it signed in as) exactly once.
pub async fn client_token(State(hub): State<HubState>, Json(req): Json<TokenReq>) -> Response {
    match hub.device_poll(&req.device_code, "client").await {
        DevicePoll::ApprovedClient { token, email } => {
            Json(json!({ "client_token": token, "email": email })).into_response()
        }
        DevicePoll::Pending => err("authorization_pending"),
        DevicePoll::SlowDown => err("slow_down"),
        DevicePoll::Denied => err("access_denied"),
        // A kind-mismatched code resolves Expired in the store (uniform).
        _ => err("expired_token"),
    }
}

#[derive(Deserialize)]
pub struct TokenReq {
    device_code: String,
}

/// `POST /api/device/token` — the host polls (unauthenticated). RFC-8628 error
/// codes on the 4xx body; the minted token on success.
pub async fn token(State(hub): State<HubState>, Json(req): Json<TokenReq>) -> Response {
    match hub.device_poll(&req.device_code, "agent").await {
        DevicePoll::Approved { token, agent_id } => {
            Json(json!({ "uplink_token": token, "agent_id": agent_id })).into_response()
        }
        DevicePoll::Pending => err("authorization_pending"),
        DevicePoll::SlowDown => err("slow_down"),
        DevicePoll::Denied => err("access_denied"),
        // Expired, plus a kind-mismatched code (which the store reports Expired).
        _ => err("expired_token"),
    }
}

fn err(code: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": code }))).into_response()
}

#[derive(Deserialize)]
pub struct ValidateReq {
    machine_id: String,
}

/// `POST /api/device/validate` — a headless host asks whether its persisted uplink
/// token is still accepted (i.e. the machine hasn't been unlinked). Authed by the
/// token itself (Bearer), exactly like the uplink; no cookie. `200` = valid,
/// `401` = revoked/unknown. This lets `--enroll` re-run the device flow instead of
/// reusing a dead token and flapping the uplink forever (proposal 0048).
pub async fn validate(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<ValidateReq>) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if hub.resolve_agent(req.machine_id.trim(), token).await.is_some() {
        StatusCode::OK.into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "revoked").into_response()
    }
}

#[derive(Deserialize)]
pub struct ApproveReq {
    user_code: String,
}

/// `POST /api/device/approve` — a logged-in browser binds the code to its tenant
/// (cookie-authed). This is the *only* place a tenant is chosen.
pub async fn approve(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<ApproveReq>) -> Response {
    let Some(user_id) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    match hub.device_approve(&user_id, &req.user_code).await {
        // `machine_id` is kept for pre-0060 frontends; `kind`+`label` (0060 B6)
        // let the /activate page say WHAT was approved (machine vs terminal).
        Ok(a) => Json(json!({ "machine_id": a.label, "kind": a.kind, "label": a.label })).into_response(),
        Err(e) => match e.to_string().strip_prefix("LIMIT:") {
            // Plan cap hit → 402 with the human message (the dashboard shows it).
            Some(msg) => (StatusCode::PAYMENT_REQUIRED, msg.to_string()).into_response(),
            None => (StatusCode::NOT_FOUND, "unknown or expired code").into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::code_rate_limited;

    // B5: the per-source window trips after the cap and never bleeds across
    // sources. (Sources here are synthetic keys — the window is keyed on
    // whatever `source_key` derives per request.)
    #[test]
    fn code_mint_window_is_per_source() {
        let src = "test-src-a";
        let mut allowed = 0;
        for _ in 0..40 {
            if !code_rate_limited(src) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, super::DEVICE_CODES_PER_MIN, "cap enforced");
        assert!(code_rate_limited(src), "still limited within the window");
        assert!(!code_rate_limited("test-src-b"), "another source is unaffected");
    }
}
