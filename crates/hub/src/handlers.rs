//! The client-facing wire contract, served by the hub. M2 exposes the read-only
//! aggregation (`/api/sessions` union + `/api/machines`) and the auth endpoints;
//! attach + lifecycle routing arrive in later milestones. The auth handlers
//! mirror the agent's (`src/handlers.rs`) but read the gate off [`HubState`].

use std::collections::HashSet;

use axum::extract::{Query, RawQuery, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use cc_screen_protocol::hub::{Cmd, CmdResult};
use cc_screen_protocol::{AuthStatus, CreateReq, DeleteReq, Favorite, LoginReq, SessionInfo};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::registry::{MachineInfo, RequestErr};
use crate::state::HubState;

// ── GET /api/sessions — union across all agents, machine-tagged ────────────────
pub async fn sessions(State(hub): State<HubState>) -> Json<Vec<SessionInfo>> {
    Json(hub.registry.all_sessions())
}

// ── GET /api/machines — for the picker + offline greying ───────────────────────
pub async fn machines(State(hub): State<HubState>) -> Json<Vec<MachineInfo>> {
    Json(hub.registry.machines())
}

// ── Auth (mirrors the agent's) ─────────────────────────────────────────────────
pub async fn login(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    let auth = &hub.client_auth;
    if auth.verify_login(&req.secret) {
        let cookie = auth.issue_cookie(cc_screen_auth::is_https(&headers));
        return (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true })))
            .into_response();
    }
    // Fixed delay to blunt guessing, as on the agent.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false }))).into_response()
}

pub async fn auth_status(
    State(hub): State<HubState>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Json<AuthStatus> {
    let auth = &hub.client_auth;
    Json(AuthStatus {
        auth_required: auth.enabled(),
        authed: !auth.enabled() || auth.is_authed(&headers, q.as_deref()),
    })
}

pub async fn logout(State(hub): State<HubState>) -> Response {
    (StatusCode::OK, [(header::SET_COOKIE, hub.client_auth.clear_cookie())]).into_response()
}

// ── Lifecycle + control, routed to the owning agent ──────────────────────────
// Each handler reads `?machine=` to pick the agent, sends a `Cmd`, awaits the
// `Reply`, and maps the `CmdResult` to the HTTP shape the client already expects.

#[derive(Deserialize)]
pub struct MachineQ {
    #[serde(default)]
    machine: String,
}

#[derive(Deserialize)]
pub struct RootQ {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    machine: String,
}

#[derive(Deserialize)]
pub struct KeyBody {
    session: String,
    key: String,
}

#[derive(Deserialize)]
pub struct PasteBody {
    session: String,
    text: String,
    #[serde(default)]
    enter: bool,
}

#[derive(Deserialize)]
pub struct SessionBody {
    session: String,
}

/// Resolve the target agent (by explicit `machine`, else by `session` owner / the
/// single online machine — for machine-less clients like the PWA), send the op,
/// await the reply. The `Err` arm is a ready-made HTTP error response.
async fn route(
    hub: &HubState,
    machine: &str,
    session: Option<&str>,
    cmd: Cmd,
) -> Result<CmdResult, Response> {
    let agent = hub.registry.resolve(machine, session).ok_or_else(|| {
        (StatusCode::NOT_FOUND, "no online machine for that request (try ?machine=)").into_response()
    })?;
    agent.request(cmd).await.map_err(|e| match e {
        RequestErr::Offline => (StatusCode::SERVICE_UNAVAILABLE, "machine offline").into_response(),
        RequestErr::Timeout => (StatusCode::GATEWAY_TIMEOUT, "agent did not respond").into_response(),
    })
}

/// Map a bare Ok/Error reply to a status (the success status varies per op).
fn ok_or_err(result: CmdResult, ok: StatusCode) -> Response {
    match result {
        CmdResult::Ok => ok.into_response(),
        CmdResult::Error { code, msg } => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
    }
}

pub async fn create(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(req): Json<CreateReq>,
) -> Response {
    // A create has no existing session to disambiguate by — route to the chosen
    // (or single online) machine.
    match route(&hub, &q.machine, None, Cmd::Create(req)).await {
        Ok(CmdResult::Created(name)) => (StatusCode::OK, Json(json!({ "name": name }))).into_response(),
        Ok(CmdResult::Error { code, msg }) => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn delete(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(req): Json<DeleteReq>,
) -> Response {
    let session = req.session.clone();
    match route(&hub, &q.machine, Some(&session), Cmd::Delete(req)).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn key(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(b): Json<KeyBody>,
) -> Response {
    let session = b.session.clone();
    match route(&hub, &q.machine, Some(&session), Cmd::Key { session: b.session, key: b.key }).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn paste(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(b): Json<PasteBody>,
) -> Response {
    let session = b.session.clone();
    let cmd = Cmd::Paste { session: b.session, text: b.text, enter: b.enter };
    match route(&hub, &q.machine, Some(&session), cmd).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn clear_history(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(b): Json<SessionBody>,
) -> Response {
    let session = b.session.clone();
    match route(&hub, &q.machine, Some(&session), Cmd::ClearHistory { session: b.session }).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn session_root(State(hub): State<HubState>, Query(q): Query<RootQ>) -> Response {
    let session = q.session.clone();
    match route(&hub, &q.machine, session.as_deref(), Cmd::SessionRoot { session: q.session }).await {
        Ok(CmdResult::SessionRoot { root, home }) => {
            Json(json!({ "root": root, "home": home })).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restorable(State(hub): State<HubState>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &q.machine, None, Cmd::Restorable).await {
        Ok(CmdResult::Restorable(list)) => Json(list).into_response(),
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restore(State(hub): State<HubState>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &q.machine, None, Cmd::Restore).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

// ── Favorites (hub-local: one list for the whole fleet) ───────────────────────
fn favorites_path(hub: &HubState) -> std::path::PathBuf {
    hub.config_dir.join("favorites.json")
}

pub async fn get_favorites(State(hub): State<HubState>) -> Json<Vec<Favorite>> {
    let list = std::fs::read_to_string(favorites_path(&hub))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Favorite>>(&s).ok())
        .unwrap_or_default();
    Json(list)
}

pub async fn put_favorites(
    State(hub): State<HubState>,
    Json(list): Json<Vec<Favorite>>,
) -> Response {
    // Same validation as the agent's store: dedupe by id, cap count + length.
    const MAX_FAV: usize = 200;
    const MAX_FAV_LEN: usize = 8000;
    let mut clean: Vec<Favorite> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in list {
        let text = f.text.trim();
        let id = f.id.trim().to_string();
        if text.is_empty() || id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let text: String = text.chars().take(MAX_FAV_LEN).collect();
        clean.push(Favorite { id, text });
        if clean.len() >= MAX_FAV {
            break;
        }
    }
    let path = favorites_path(&hub);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(&clean).unwrap_or_default();
    match std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
        Ok(()) => Json(clean).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── File browser / editor (small ops), routed to the owning agent ────────────
// Bulk transfers (download / upload / clipboard image) are NOT here — they use
// the dedicated bulk stream (later milestone).

#[derive(Deserialize)]
pub struct FileGetQ {
    #[serde(default)]
    path: String,
    #[serde(default)]
    session: String,
    #[serde(default)]
    machine: String,
}

/// Route a file op (resolving the machine by `session` owner / single online box
/// when the PWA omits it) and map its `CmdResult` to JSON / 204 / error.
async fn file_route(
    hub: &HubState,
    machine: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
) -> Response {
    match route(hub, machine, session, Cmd::File { op: op.to_string(), args }).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(CmdResult::Ok) => StatusCode::NO_CONTENT.into_response(),
        Ok(CmdResult::Error { code, msg }) => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

// `dirs`/`files` can disambiguate by the session whose cwd is being browsed;
// otherwise (and for the path-only ops) we fall back to the single online machine.
fn opt(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub async fn dirs(State(hub): State<HubState>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &q.machine, opt(&q.session), "dirs", json!({ "path": q.path, "session": q.session })).await
}

pub async fn files(State(hub): State<HubState>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &q.machine, opt(&q.session), "files", json!({ "path": q.path, "session": q.session })).await
}

pub async fn file_read(State(hub): State<HubState>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &q.machine, opt(&q.session), "read", json!({ "path": q.path })).await
}

// POST handlers forward the client's JSON body straight through as the op args;
// path-only, so they route to the explicit (or single online) machine.
pub async fn file_write(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &q.machine, None, "write", body).await
}

pub async fn file_delete(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &q.machine, None, "delete", body).await
}

pub async fn mkdir(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &q.machine, None, "mkdir", body).await
}

pub async fn rmdir(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &q.machine, None, "rmdir", body).await
}

pub async fn rename(
    State(hub): State<HubState>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &q.machine, None, "rename", body).await
}

// ── Web Push (hub-local: one VAPID key + sub store for the whole fleet) ───────
pub async fn push_key(State(hub): State<HubState>) -> Json<Value> {
    Json(json!({ "key": hub.push.application_server_key() }))
}

#[derive(Deserialize)]
pub struct SubscribeReq {
    endpoint: String,
    keys: SubKeys,
}
#[derive(Deserialize)]
pub struct SubKeys {
    p256dh: String,
    auth: String,
}

pub async fn push_subscribe(State(hub): State<HubState>, Json(req): Json<SubscribeReq>) -> Response {
    if req.endpoint.is_empty() || req.keys.p256dh.is_empty() || req.keys.auth.is_empty() {
        return (StatusCode::BAD_REQUEST, "incomplete subscription").into_response();
    }
    hub.push.add_sub(cc_screen_push::StoredSub {
        endpoint: req.endpoint,
        p256dh: req.keys.p256dh,
        auth: req.keys.auth,
    });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct UnsubscribeReq {
    endpoint: String,
}

pub async fn push_unsubscribe(
    State(hub): State<HubState>,
    Json(req): Json<UnsubscribeReq>,
) -> Response {
    hub.push.remove_sub(&req.endpoint);
    StatusCode::NO_CONTENT.into_response()
}

pub async fn push_test(State(hub): State<HubState>) -> Response {
    hub.push.notify("cc-screen", "🔔 Test buzz — notifications are on", "").await;
    StatusCode::NO_CONTENT.into_response()
}

/// Gate every `/api/*` route except the auth endpoints; non-`/api/` paths
/// (notably `/agent/ws`, which has its own per-agent token check) are exempt. A
/// no-op when no client credential is configured.
pub async fn require_client_auth(State(hub): State<HubState>, req: Request, next: Next) -> Response {
    let auth = &hub.client_auth;
    if !auth.enabled() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    let exempt = !path.starts_with("/api/")
        || matches!(path, "/api/login" | "/api/auth" | "/api/logout");
    if exempt || auth.is_authed(req.headers(), req.uri().query()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}
