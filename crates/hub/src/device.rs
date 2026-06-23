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

/// `POST /api/device/code` — the headless host requests a code (unauthenticated).
pub async fn code(State(hub): State<HubState>, Json(req): Json<CodeReq>) -> Response {
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
pub struct TokenReq {
    device_code: String,
}

/// `POST /api/device/token` — the host polls (unauthenticated). RFC-8628 error
/// codes on the 4xx body; the minted token on success.
pub async fn token(State(hub): State<HubState>, Json(req): Json<TokenReq>) -> Response {
    match hub.device_poll(&req.device_code).await {
        DevicePoll::Approved { token, agent_id } => {
            Json(json!({ "uplink_token": token, "agent_id": agent_id })).into_response()
        }
        DevicePoll::Pending => err("authorization_pending"),
        DevicePoll::SlowDown => err("slow_down"),
        DevicePoll::Denied => err("access_denied"),
        DevicePoll::Expired => err("expired_token"),
    }
}

fn err(code: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": code }))).into_response()
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
        Ok(machine_id) => Json(json!({ "machine_id": machine_id })).into_response(),
        Err(e) => match e.to_string().strip_prefix("LIMIT:") {
            // Plan cap hit → 402 with the human message (the dashboard shows it).
            Some(msg) => (StatusCode::PAYMENT_REQUIRED, msg.to_string()).into_response(),
            None => (StatusCode::NOT_FOUND, "unknown or expired code").into_response(),
        },
    }
}
