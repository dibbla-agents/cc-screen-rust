//! Account + machine-management endpoints for the dashboard (proposal 0001
//! Phase 3), compiled only under `multi-tenant`. Public signup, and the
//! owner-scoped agent list / unlink / token-rotate the dashboard drives.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::state::HubState;

#[derive(Deserialize)]
pub struct SignupReq {
    email: String,
    password: String,
}

/// The one message every server-side signup failure returns (proposal 0053 Part
/// E / [0042]): a duplicate email is byte-identical to any other create failure,
/// so an anonymous caller can't probe which addresses have accounts.
const SIGNUP_GENERIC_ERROR: &str =
    "could not create the account — if you already have one, sign in instead";

/// How many signup attempts one source gets per rolling minute before a 429.
/// Generous enough for the whole-journey E2E (one signup + retries), tight
/// enough that a scripted burst is stopped (proposal 0053 Part E item 2).
const SIGNUP_PER_MIN: usize = 5;

/// Per-source sliding-window rate limit for signup, independent of the shared
/// login backoff (which only reacts to *failures* — an unauthenticated
/// account-creation endpoint must also bound *successful* bursts). Process-wide;
/// bounded; prunes as it goes.
fn signup_rate_limited(source: &str) -> bool {
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
    if hits.len() >= SIGNUP_PER_MIN {
        return true;
    }
    hits.push(now);
    false
}

/// `POST /api/signup` — create an account and log the user straight in (mint the
/// identity cookie). Unauthenticated (it's how you get an account). Rate-limited
/// per source (plus the shared login backoff), and enumeration-safe: every
/// server-side failure answers the same status + body (proposal 0053 Part E).
pub async fn signup(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<SignupReq>) -> Response {
    if !hub.multi_tenant() {
        return (StatusCode::NOT_IMPLEMENTED, "not a multi-tenant hub").into_response();
    }
    if !crate::handlers::password_login_enabled() {
        return (StatusCode::FORBIDDEN, Json(json!({ "ok": false, "error": "sign-up disabled — use Google" }))).into_response();
    }
    let source = cc_screen_auth::source_key(&headers);
    let now = std::time::Instant::now();
    if hub.login_throttle.locked_for(&source, now).is_some() || signup_rate_limited(&source) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "ok": false, "error": "too many attempts" }))).into_response();
    }
    // Input-policy failures are deterministic from the request alone (no account
    // knowledge involved), so they may keep a specific message + status.
    if req.email.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "email is required" }))).into_response();
    }
    if req.password.len() < crate::db::MIN_PASSWORD_LEN {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": format!("password must be at least {} characters", crate::db::MIN_PASSWORD_LEN) })),
        )
            .into_response();
    }
    match hub.create_user(&req.email, &req.password).await {
        Ok(user_id) => {
            // Land any email invites waiting for this address (proposal 0056 C3)
            // — they show in the new account's inbox on its first poll.
            hub.attach_email_invites(&user_id, &req.email).await;
            hub.login_throttle.record_success(&source);
            let cookie = hub.client_auth.issue_cookie_for(&user_id, cc_screen_auth::is_https(&headers));
            (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
        }
        Err(_) => {
            hub.login_throttle.record_failure(&source, now);
            // One generic answer for every server-side failure — a duplicate
            // email is indistinguishable from any other create error ([0042]).
            (StatusCode::CONFLICT, Json(json!({ "ok": false, "error": SIGNUP_GENERIC_ERROR }))).into_response()
        }
    }
}

/// `GET /api/agents` — the caller's registered machines, each annotated with live
/// online status from the registry.
pub async fn list(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(user_id) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let out: Vec<_> = hub
        .list_agents(&user_id)
        .await
        .into_iter()
        .map(|a| {
            let conn = hub.registry.get(&a.agent_id);
            let online = conn.as_ref().is_some_and(|c| c.online());
            // Which coding assistants this machine is short of (proposal 0050
            // D3) — so the dashboard can say `⚠ 2 missing · Install` without N
            // extra round-trips. Empty (and omitted client-side) when it's
            // complete or offline; kept honest by `AgentMsg::Tools`.
            let missing: Vec<String> = conn.as_ref().map(|c| c.missing_tools()).unwrap_or_default();
            json!({
                "agentId": a.agent_id,
                "machine": a.machine_id,
                "online": online,
                "createdAt": a.created_at,
                "missing": missing,
            })
        })
        .collect();
    Json(out).into_response()
}

#[derive(Deserialize)]
pub struct UnlinkReq {
    agent_id: String,
}

/// `POST /api/agents/unlink` — remove one of the caller's machines (owner-scoped).
pub async fn unlink(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<UnlinkReq>) -> Response {
    let Some(user_id) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    if hub.delete_agent(&user_id, &req.agent_id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such machine").into_response()
    }
}

#[derive(Deserialize)]
pub struct RotateReq {
    machine: String,
}

/// `POST /api/agents/rotate` — mint a fresh uplink token for the caller's machine
/// (the old one dies immediately). Returned **once** so the operator can
/// reconfigure the box (or it re-enrolls).
pub async fn rotate(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<RotateReq>) -> Response {
    let Some(user_id) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    if req.machine.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "machine required").into_response();
    }
    match hub.rotate_agent(&user_id, &req.machine).await {
        Ok(token) => Json(json!({ "token": token })).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "rotate failed").into_response(),
    }
}

// The owner-scoped sharing surface moved to `share.rs` (proposal 0040): an invite
// must be accepted before it grants, so creation no longer lands a grant directly.
// The 0039 grant store (`shares`) is now written only by an accepted invite.
