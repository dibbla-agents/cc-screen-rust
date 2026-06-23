//! The client-facing wire contract, served by the hub. M2 exposes the read-only
//! aggregation (`/api/sessions` union + `/api/machines`) and the auth endpoints;
//! attach + lifecycle routing arrive in later milestones. The auth handlers
//! mirror the agent's (`src/handlers.rs`) but read the gate off [`HubState`].

use std::collections::HashSet;

use axum::extract::{Query, RawQuery, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use cc_screen_protocol::hub::{Cmd, CmdResult};
use cc_screen_protocol::{AuthStatus, CreateReq, DeleteReq, Favorite, LoginReq, SessionInfo, ToolInfo};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::registry::{MachineInfo, RequestErr, UserScope};
use crate::state::HubState;

// ── GET /api/sessions — union across the caller's agents, machine-tagged ───────
pub async fn sessions(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
) -> Json<Vec<SessionInfo>> {
    Json(hub.registry.all_sessions_for(&scope))
}

// ── GET /api/machines — for the picker + offline greying (caller's agents) ─────
pub async fn machines(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
) -> Json<Vec<MachineInfo>> {
    Json(hub.registry.machines_for(&scope))
}

// ── GET /api/tools — the chosen agent's tool list (used by New Session) ─────────
// Agents register their tools at uplink time, so the registry already has them —
// no round-trip to the agent needed. Resolves the explicit `?machine=`, else the
// single online agent; `[]` when unknown/offline (which leaves New Session's
// Create disabled, same as a tool-less agent).
pub async fn tools(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
) -> Json<Vec<ToolInfo>> {
    let tools = hub
        .registry
        .resolve_scoped(&scope, &q.machine, None)
        .map(|a| a.tools.clone())
        .unwrap_or_default();
    Json(tools)
}

// ── Auth (mirrors the agent's) ─────────────────────────────────────────────────
pub async fn login(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    let auth = &hub.client_auth;
    let source = cc_screen_auth::source_key(&headers);
    let now = std::time::Instant::now();
    if hub.login_throttle.locked_for(&source, now).is_some() {
        return (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "ok": false, "error": "too many attempts" }))).into_response();
    }
    // Multi-tenant (proposal 0001 §3.2): look the account up by email and verify
    // `secret` as that user's argon2 password, minting an identity-carrying cookie.
    // The single-tenant shared-secret path below is untouched.
    if hub.multi_tenant() {
        let email = req.email.as_deref().unwrap_or("");
        if !email.trim().is_empty() {
            if let Some(user_id) = hub.verify_login(email, &req.secret).await {
                hub.login_throttle.record_success(&source);
                let cookie = auth.issue_cookie_for(&user_id, cc_screen_auth::is_https(&headers));
                return (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true })))
                    .into_response();
            }
        }
        hub.login_throttle.record_failure(&source, now);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false }))).into_response();
    }
    if auth.verify_login(&req.secret) {
        hub.login_throttle.record_success(&source);
        let cookie = auth.issue_cookie(cc_screen_auth::is_https(&headers));
        return (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true })))
            .into_response();
    }
    hub.login_throttle.record_failure(&source, now);
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

/// `GET /api/me` — the boot/identity read for the web UI (proposal 0001 §5).
/// Always 200. `multiTenant` tells the frontend which login model to render;
/// `googleEnabled` whether to show the Google button; when multi-tenant and the
/// session cookie is valid, the logged-in account. Single-tenant reports
/// `multiTenant:false` and the frontend falls back to the `/api/auth` gate.
/// Exempt from the auth gate so it can answer "who am I?" with no session.
pub async fn me(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let multi = hub.multi_tenant();
    let google = multi && google_enabled();
    if multi {
        if let Some(user_id) = hub.client_auth.user_from_cookie(&headers) {
            if let Some(email) = hub.user_email(&user_id).await {
                return Json(json!({
                    "multiTenant": true, "googleEnabled": google,
                    "authenticated": true, "userId": user_id, "email": email,
                }))
                .into_response();
            }
        }
    }
    Json(json!({ "multiTenant": multi, "googleEnabled": google, "authenticated": false }))
        .into_response()
}

#[cfg(feature = "multi-tenant")]
fn google_enabled() -> bool {
    crate::oauth::is_configured()
}
#[cfg(not(feature = "multi-tenant"))]
fn google_enabled() -> bool {
    false
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
    scope: &UserScope,
    machine: &str,
    session: Option<&str>,
    cmd: Cmd,
) -> Result<CmdResult, Response> {
    let agent = hub.registry.resolve_scoped(scope, machine, session).ok_or_else(|| {
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
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(req): Json<CreateReq>,
) -> Response {
    // Plan gate (proposal 0001 Phase 4): cap concurrent sessions per tenant.
    // Multi-tenant only; single-tenant has no per-user limits.
    #[cfg(feature = "multi-tenant")]
    if let UserScope::User(uid) = &scope {
        let limits = hub.limits_for(uid).await;
        let current = hub.registry.all_sessions_for(&scope).len() as i64;
        if current >= limits.max_concurrent_sessions {
            return (
                StatusCode::PAYMENT_REQUIRED,
                format!("Session limit reached for your plan ({}).", limits.max_concurrent_sessions),
            )
                .into_response();
        }
    }
    // A create has no existing session to disambiguate by — route to the chosen
    // (or single online) machine.
    match route(&hub, &scope, &q.machine, None, Cmd::Create(req)).await {
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
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(req): Json<DeleteReq>,
) -> Response {
    let session = req.session.clone();
    match route(&hub, &scope, &q.machine, Some(&session), Cmd::Delete(req)).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn key(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(b): Json<KeyBody>,
) -> Response {
    let session = b.session.clone();
    match route(&hub, &scope, &q.machine, Some(&session), Cmd::Key { session: b.session, key: b.key }).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn paste(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(b): Json<PasteBody>,
) -> Response {
    let session = b.session.clone();
    let cmd = Cmd::Paste { session: b.session, text: b.text, enter: b.enter };
    match route(&hub, &scope, &q.machine, Some(&session), cmd).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

pub async fn clear_history(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(b): Json<SessionBody>,
) -> Response {
    let session = b.session.clone();
    match route(&hub, &scope, &q.machine, Some(&session), Cmd::ClearHistory { session: b.session }).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

#[derive(Deserialize)]
pub struct ColorBody {
    session: String,
    #[serde(default)]
    color: Option<String>,
}

// Set/clear a session's mark colour (proposal 0029), routed to the owning agent;
// the agent replies with the updated SessionInfo as JSON.
pub async fn set_color(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(b): Json<ColorBody>,
) -> Response {
    let session = b.session.clone();
    let cmd = Cmd::SetColor { session: b.session, color: b.color };
    match route(&hub, &scope, &q.machine, Some(&session), cmd).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(CmdResult::Error { code, msg }) => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

#[derive(Deserialize)]
pub struct LabelBody {
    session: String,
    #[serde(default)]
    label: Option<String>,
}

// Set/clear a session's display label (proposal 0035), routed to the owning agent;
// the agent replies with the updated SessionInfo as JSON.
pub async fn set_label(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(b): Json<LabelBody>,
) -> Response {
    let session = b.session.clone();
    let cmd = Cmd::SetLabel { session: b.session, label: b.label };
    match route(&hub, &scope, &q.machine, Some(&session), cmd).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(CmdResult::Error { code, msg }) => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn session_root(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<RootQ>) -> Response {
    let session = q.session.clone();
    match route(&hub, &scope, &q.machine, session.as_deref(), Cmd::SessionRoot { session: q.session }).await {
        Ok(CmdResult::SessionRoot { root, home }) => {
            Json(json!({ "root": root, "home": home })).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restorable(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &scope, &q.machine, None, Cmd::Restorable).await {
        Ok(CmdResult::Restorable(list)) => Json(list).into_response(),
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restore(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &scope, &q.machine, None, Cmd::Restore).await {
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
    scope: &UserScope,
    machine: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
) -> Response {
    match route(hub, scope, machine, session, Cmd::File { op: op.to_string(), args }).await {
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

pub async fn dirs(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &scope, &q.machine, opt(&q.session), "dirs", json!({ "path": q.path, "session": q.session })).await
}

pub async fn files(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &scope, &q.machine, opt(&q.session), "files", json!({ "path": q.path, "session": q.session })).await
}

// Recursive fuzzy dir search (proposal 0016), per-agent like `dirs`: the chosen
// machine searches its own $HOME. `?session=` disambiguates the owner when the
// PWA omits `?machine=` (falls back to the single online box).
#[derive(Deserialize)]
pub struct DirSearchQ {
    #[serde(default)]
    q: String,
    #[serde(default)]
    root: String,
    #[serde(default)]
    session: String,
    #[serde(default)]
    machine: String,
}

pub async fn dirs_search(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(qy): Query<DirSearchQ>) -> Response {
    file_route(&hub, &scope, &qy.machine, opt(&qy.session), "dirs_search", json!({ "q": qy.q, "root": qy.root })).await
}

// Recursive fuzzy *file* search (proposal 0027), per-agent like `dirs_search`:
// the chosen machine searches its own $HOME. `?session=` both disambiguates the
// owning agent and lets that agent default the root to the session's project.
pub async fn files_search(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(qy): Query<DirSearchQ>) -> Response {
    file_route(
        &hub,
        &scope,
        &qy.machine,
        opt(&qy.session),
        "files_search",
        json!({ "q": qy.q, "root": qy.root, "session": qy.session }),
    )
    .await
}

pub async fn file_read(State(hub): State<HubState>, Extension(scope): Extension<UserScope>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &scope, &q.machine, opt(&q.session), "read", json!({ "path": q.path })).await
}

// POST handlers forward the client's JSON body straight through as the op args;
// path-only, so they route to the explicit (or single online) machine.
pub async fn file_write(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "write", body).await
}

pub async fn file_delete(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "delete", body).await
}

pub async fn mkdir(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "mkdir", body).await
}

pub async fn rmdir(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "rmdir", body).await
}

pub async fn rename(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "rename", body).await
}

pub async fn move_path(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "move", body).await
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

pub async fn push_subscribe(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Json(req): Json<SubscribeReq>,
) -> Response {
    if req.endpoint.is_empty() || req.keys.p256dh.is_empty() || req.keys.auth.is_empty() {
        return (StatusCode::BAD_REQUEST, "incomplete subscription").into_response();
    }
    // Stamp the owning tenant (§10.6.1) so this device only ever receives this
    // user's notifications. `None` in single-tenant → unscoped, as before.
    let owner = if hub.multi_tenant() {
        hub.client_auth.user_from_cookie(&headers)
    } else {
        None
    };
    hub.push.add_sub(cc_screen_push::StoredSub {
        endpoint: req.endpoint,
        p256dh: req.keys.p256dh,
        auth: req.keys.auth,
        owner,
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

pub async fn push_test(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    // Buzz only the caller's own devices in multi-tenant (§10.6.1).
    let owner = if hub.multi_tenant() { hub.client_auth.user_from_cookie(&headers) } else { None };
    hub.push
        .notify_scoped(owner.as_deref(), "cc-screen", "🔔 Test buzz — notifications are on", "")
        .await;
    StatusCode::NO_CONTENT.into_response()
}

/// Gate every `/api/*` route except the auth endpoints; non-`/api/` paths
/// (notably `/agent/ws`, which has its own per-agent token check) are exempt. A
/// no-op when no client credential is configured.
pub async fn require_client_auth(State(hub): State<HubState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // Browser trust boundary — runs regardless of the auth gate. The `/agent/*`
    // uplink + bulk dial-backs are not browser-facing (non-`/api/`), so skip them.
    if path.starts_with("/api/") && !hub.origin.check(req.headers()) {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }
    let exempt = !path.starts_with("/api/")
        || matches!(path.as_str(), "/api/login" | "/api/signup" | "/api/auth" | "/api/me" | "/api/logout")
        // The Google OAuth login flow (start/callback) must be reachable without a
        // session — it IS the login.
        || path.starts_with("/api/auth/google/")
        // Device-flow host endpoints are unauthenticated (the device_code is the
        // bearer); /api/device/approve is intentionally NOT exempt — it needs the
        // user's session to bind the enrollment to their tenant.
        || matches!(path.as_str(), "/api/device/code" | "/api/device/token");

    // Multi-tenant (proposal 0001 §4.1): identity comes from the session cookie,
    // not the shared secret. A gated request without a valid session is refused
    // here; the resolved tenant scope is stashed for the handlers so every relay
    // lookup is filtered to the caller's own agents.
    if hub.multi_tenant() {
        let user = hub.client_auth.user_from_cookie(req.headers());
        if !exempt && user.is_none() {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
        // Exempt paths with no session get a scope that matches no agent (harmless
        // — they don't consult it); gated paths always have a real user here.
        let scope = user.map(UserScope::User).unwrap_or_else(|| UserScope::User(String::new()));
        req.extensions_mut().insert(scope);
        return next.run(req).await;
    }

    // Single-tenant: every authed client sees every agent (today's behavior).
    req.extensions_mut().insert(UserScope::All);
    let auth = &hub.client_auth;
    if !auth.enabled() {
        return next.run(req).await;
    }
    if exempt || auth.is_authed(req.headers(), req.uri().query()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}
