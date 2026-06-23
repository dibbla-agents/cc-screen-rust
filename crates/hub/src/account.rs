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

/// `POST /api/signup` — create an account and log the user straight in (mint the
/// identity cookie). Unauthenticated (it's how you get an account). Throttled with
/// the shared login throttle to blunt abuse.
pub async fn signup(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<SignupReq>) -> Response {
    if !hub.multi_tenant() {
        return (StatusCode::NOT_IMPLEMENTED, "not a multi-tenant hub").into_response();
    }
    if !crate::handlers::password_login_enabled() {
        return (StatusCode::FORBIDDEN, Json(json!({ "ok": false, "error": "sign-up disabled — use Google" }))).into_response();
    }
    let source = cc_screen_auth::source_key(&headers);
    let now = std::time::Instant::now();
    if hub.login_throttle.locked_for(&source, now).is_some() {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "ok": false, "error": "too many attempts" }))).into_response();
    }
    match hub.create_user(&req.email, &req.password).await {
        Ok(user_id) => {
            hub.login_throttle.record_success(&source);
            let cookie = hub.client_auth.issue_cookie_for(&user_id, cc_screen_auth::is_https(&headers));
            (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            hub.login_throttle.record_failure(&source, now);
            // Most failures are a duplicate email or a too-short password — 409 with
            // a human message the signup form can show.
            (StatusCode::CONFLICT, Json(json!({ "ok": false, "error": e.to_string() }))).into_response()
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
            let online = hub.registry.is_online(&a.agent_id);
            json!({ "agentId": a.agent_id, "machine": a.machine_id, "online": online, "createdAt": a.created_at })
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
