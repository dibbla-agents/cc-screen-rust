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

use crate::registry::{MachineInfo, RequestErr, Visibility};
use crate::state::HubState;

// ── GET /api/sessions — union across the caller's agents, machine-tagged ───────
pub async fn sessions(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
) -> Json<Vec<SessionInfo>> {
    Json(hub.registry.all_sessions_for(&scope))
}

// ── GET /api/machines — for the picker + offline greying (caller's agents) ─────
pub async fn machines(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
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
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
) -> Json<Vec<ToolInfo>> {
    let tools = hub
        .registry
        .resolve_scoped(&scope, &q.machine, None)
        .map(|a| a.tools())
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
        if !password_login_enabled() {
            return (StatusCode::FORBIDDEN, Json(json!({ "ok": false, "error": "password login disabled — use Google" }))).into_response();
        }
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
// `headers` is only read by the multi-tenant branch below.
#[cfg_attr(not(feature = "multi-tenant"), allow(unused_variables))]
pub async fn me(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let multi = hub.multi_tenant();
    let google = multi && google_enabled();
    let password = password_login_enabled();
    #[cfg(feature = "multi-tenant")]
    if multi {
        if let Some(user_id) = hub.client_auth.user_from_cookie(&headers) {
            if let Some(email) = hub.user_email(&user_id).await {
                // Plan facts for the limit-card UX (proposal 0056 B1): the plan's
                // name + caps and the current agent count, plus the operator's
                // support address (CCHUB_SUPPORT_EMAIL) for the upgrade mailto.
                let limits = hub.limits_for(&user_id).await;
                let support = std::env::var("CCHUB_SUPPORT_EMAIL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                // Org membership (proposal 0063 B1): the caller's org, if any —
                // the minimal contract TeamCard + the audit view render from.
                let membership = match hub.store() {
                    Some(s) => s.org_for_user(&user_id).await,
                    None => None,
                };
                let mut org_block: Option<serde_json::Value> = None;
                let mut plan = serde_json::Map::new();
                plan.insert("name".into(), json!(limits.plan));
                plan.insert("maxAgents".into(), json!(limits.max_agents));
                plan.insert("maxSessions".into(), json!(limits.max_concurrent_sessions));
                match (&membership, hub.store()) {
                    (Some((org, role)), Some(store)) => {
                        let members = store.org_members(&org.id).await;
                        let owner_email = members.iter().find(|m| m.role == "owner").map(|m| m.email.clone());
                        org_block = Some(json!({
                            "id": org.id, "name": org.name, "role": role,
                            "seats": org.seat_count, "memberCount": members.len(),
                        }));
                        if org.seat_count > 0 {
                            // Pool-governed (proposal 0064 Part C): the plan block
                            // reports org truth — pooled caps, org-wide machine
                            // count, plus seats/members/orgId for the seats UI.
                            let member_ids = store.org_member_ids(&org.id).await;
                            plan.insert("agents".into(), json!(store.agent_count_for_users(&member_ids).await));
                            plan.insert("seats".into(), json!(org.seat_count));
                            plan.insert("members".into(), json!(members.len() as i64));
                            plan.insert("orgId".into(), json!(org.id));
                            plan.insert("orgName".into(), json!(org.name));
                            plan.insert("orgRole".into(), json!(role));
                            if let Some(oe) = owner_email {
                                plan.insert("ownerEmail".into(), json!(oe));
                            }
                            if let Some(st) = &org.plan_status {
                                plan.insert("status".into(), json!(st));
                            }
                            if let Some(pe) = org.current_period_end {
                                plan.insert("periodEnd".into(), json!(pe));
                            }
                        }
                    }
                    _ => {}
                }
                // Personal billing facts, byte-for-byte today's shape, whenever
                // the caller is NOT pool-governed (0064 acceptance 12).
                if !plan.contains_key("agents") {
                    plan.insert("agents".into(), json!(hub.agent_count(&user_id).await));
                    let (status, period_end) = match hub.store() {
                        Some(s) => s.billing_status(&user_id).await,
                        None => (None, None),
                    };
                    if let Some(st) = status {
                        plan.insert("status".into(), json!(st));
                    }
                    if let Some(pe) = period_end {
                        plan.insert("periodEnd".into(), json!(pe));
                    }
                }
                let mut body = json!({
                    "multiTenant": true, "googleEnabled": google, "passwordLogin": password,
                    "authenticated": true, "userId": user_id, "email": email,
                    "plan": plan,
                    "supportEmail": support,
                    "billing": billing_enabled(),
                    // Can this hub emit mail (proposal 0073 D1)? A per-hub
                    // capability, identical for every caller and saying nothing
                    // about any address — the same shape as `billing: false`
                    // telling the seat-checkout button not to render. Deliberately
                    // NOT in the unauthenticated body below: /api/me is exempt from
                    // the auth gate, and there is no reason to tell the open
                    // internet that this hub sends mail from an authenticated
                    // domain.
                    "mail": hub.mailer.active(),
                });
                if let Some(org) = org_block {
                    body["org"] = org;
                }
                return Json(body).into_response();
            }
        }
    }
    Json(json!({
        "multiTenant": multi, "googleEnabled": google, "passwordLogin": password,
        "authenticated": false, "billing": billing_enabled(),
    }))
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

/// Whether Stripe self-serve billing is configured (proposal 0058 B4) — the
/// `billing` flag `/api/me` returns so the PWA renders checkout vs the mailto
/// fallback. Always false without the `multi-tenant` feature.
#[cfg(feature = "multi-tenant")]
fn billing_enabled() -> bool {
    crate::billing::is_configured()
}
#[cfg(not(feature = "multi-tenant"))]
fn billing_enabled() -> bool {
    false
}

/// Whether email+password login/signup is offered (multi-tenant). Off when
/// `CCHUB_OAUTH_ONLY` is set — a Google-only deployment. The frontend hides the
/// password form, and `/api/login`/`/api/signup` refuse the password path.
pub fn password_login_enabled() -> bool {
    !std::env::var("CCHUB_OAUTH_ONLY")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
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

/// What a failed `resolve_*` actually means. The old wording ("no online machine
/// for that request") named only ONE of the three reasons the resolver returns
/// `None`, and it named the one that is usually false: a session-share grantee
/// opening a file got told the machine was offline while it sat plainly online in
/// the same UI. An authorization refusal must not describe itself as an outage.
const UNRESOLVED: &str =
    "no machine matched that request — it may be offline, ambiguous, or outside what you have access to (try ?machine=)";

/// Resolve the target agent (by explicit `machine`, else by `session` owner / the
/// single online machine — for machine-less clients like the PWA), send the op,
/// await the reply. The `Err` arm is a ready-made HTTP error response.
async fn route(
    hub: &HubState,
    scope: &Visibility,
    machine: &str,
    session: Option<&str>,
    cmd: Cmd,
) -> Result<CmdResult, Response> {
    let agent = hub
        .registry
        .resolve_scoped(scope, machine, session)
        .ok_or_else(|| (StatusCode::NOT_FOUND, UNRESOLVED).into_response())?;
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
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(req): Json<CreateReq>,
) -> Response {
    // Plan gate (proposal 0001 Phase 4): cap concurrent sessions per tenant.
    // Multi-tenant only; single-tenant has no per-user limits. For an active-org
    // member the count is the POOL's (proposal 0063 C3): live sessions on agents
    // owned by any org member — sessions shared INTO a member from outside the
    // org ride someone else's agent and never count.
    #[cfg(feature = "multi-tenant")]
    if let Visibility::User(v) = &scope {
        let uid = &v.user_id;
        let limits = hub.limits_for(uid).await;
        let pool = match hub.store() {
            Some(store) => match store.org_for_user(uid).await {
                Some((org, _)) if org.seat_count > 0 => {
                    Some(store.org_member_ids(&org.id).await.into_iter().collect::<HashSet<String>>())
                }
                _ => None,
            },
            None => None,
        };
        let (current, message) = match &pool {
            Some(members) => {
                let n = hub.registry.sessions_count_owned_by(members);
                (
                    n,
                    format!(
                        "Team session limit reached ({} of {} across your team).",
                        n, limits.max_concurrent_sessions
                    ),
                )
            }
            None => (
                hub.registry.all_sessions_for(&scope).len() as i64,
                format!("Session limit reached for your plan ({}).", limits.max_concurrent_sessions),
            ),
        };
        if current >= limits.max_concurrent_sessions {
            return (StatusCode::PAYMENT_REQUIRED, message).into_response();
        }
    }
    // A create has no existing session to disambiguate by — route to the chosen
    // (or single online) machine.
    match route(&hub, &scope, &q.machine, None, Cmd::Create(req)).await {
        Ok(CmdResult::Created(name)) => {
            // Creator attribution (proposal 0039 §4.1): if a non-owner (an
            // agent-grantee) created this, stamp it so the owner's list honours the
            // share asymmetry (owner sees it only under owner-peek).
            #[cfg(feature = "multi-tenant")]
            if let Visibility::User(v) = &scope {
                if let Some(agent) = hub.registry.resolve_scoped(&scope, &q.machine, None) {
                    if agent.user_id != v.user_id {
                        agent.set_creator(&name, &v.user_id);
                    }
                }
            }
            (StatusCode::OK, Json(json!({ "name": name }))).into_response()
        }
        Ok(CmdResult::Error { code, msg }) => {
            (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn delete(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(req): Json<DeleteReq>,
) -> Response {
    let session = req.session.clone();
    match route(&hub, &scope, &q.machine, Some(&session), Cmd::Delete(req)).await {
        Ok(r) => {
            // Drop any creator attribution so a later same-named session isn't
            // mis-attributed to the deleted one's creator (proposal 0039 §4.1).
            #[cfg(feature = "multi-tenant")]
            if let Some(agent) = hub.registry.resolve_scoped(&scope, &q.machine, Some(&session)) {
                agent.forget_creator(&session);
            }
            ok_or_err(r, StatusCode::NO_CONTENT)
        }
        Err(resp) => resp,
    }
}

pub async fn key(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
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
    Extension(scope): Extension<Visibility>,
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
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(b): Json<SessionBody>,
) -> Response {
    let session = b.session.clone();
    match route(&hub, &scope, &q.machine, Some(&session), Cmd::ClearHistory { session: b.session }).await {
        Ok(r) => ok_or_err(r, StatusCode::NO_CONTENT),
        Err(resp) => resp,
    }
}

// ── Restart one session, routed to the owning agent (proposal 0087) ──────────
// Deliberately NOT the owner-only gate the assistant-update ops use. [0049] E3
// reasoned that "updating CLIs and restarting terminals" is administration, and
// that is right for the *fleet* action: it mutates machine-global binaries. A
// single session's restart mutates nothing machine-global — one child process
// exits and relaunches — and it is behaviourally a safer version of two things a
// session-share grantee can already do (type `/exit` into the PTY; POST
// /api/session/delete with mode "exit"), except that it PRESERVES the session
// where those destroy it. Denying it to someone who can destroy would be gate
// theatre. So it pins to `may_see_session`, exactly like delete/key/paste — which
// is what `resolve_scoped(.., Some(session))` enforces.
//
// The other half of the gate is the capability: an agent that predates 0087
// can't deserialize `Cmd::RestartSession`, so routing one would hang until the
// relay timed out into a 504. We answer 501 with something actionable instead.
pub async fn restart_session(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(b): Json<SessionBody>,
) -> Response {
    let agent = match hub.registry.resolve_scoped(&scope, &q.machine, Some(&b.session)) {
        Some(a) => a,
        None => return (StatusCode::NOT_FOUND, UNRESOLVED).into_response(),
    };
    if !agent.caps.iter().any(|c| c == cc_screen_protocol::hub::CAP_SESSION_RESTART) {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "this machine's agent is too old to restart a single session — run `cc-screen-rust update` on it",
        )
            .into_response();
    }
    // `update_send` is the generic "reply is JSON or an error status" mapper —
    // the restart's one-row `SessionRestartStatus` rides it unchanged.
    update_send(agent, Cmd::RestartSession { session: b.session }).await
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
    Extension(scope): Extension<Visibility>,
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
    Extension(scope): Extension<Visibility>,
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

pub async fn session_root(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<RootQ>) -> Response {
    let session = q.session.clone();
    match route(&hub, &scope, &q.machine, session.as_deref(), Cmd::SessionRoot { session: q.session }).await {
        Ok(CmdResult::SessionRoot { root, home }) => {
            Json(json!({ "root": root, "home": home })).into_response()
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restorable(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &scope, &q.machine, None, Cmd::Restorable).await {
        Ok(CmdResult::Restorable(list)) => Json(list).into_response(),
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

pub async fn restore(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<MachineQ>) -> Response {
    match route(&hub, &scope, &q.machine, None, Cmd::Restore).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(resp) => resp,
    }
}

// ── Assistant updates, routed to the owning agent (proposal 0049) ────────────
// Two ops with a stricter gate than the rest of the relay: updating CLIs and
// restarting terminals is an administrative act on someone's host, and under
// 0039 a share grants *use*, not administration. So these require OWNERSHIP —
// `may_use_agent` (which an agent-grantee satisfies) is deliberately not enough.
// The other half of the gate is the agent's advertised capability: an older
// agent can't deserialize these `Cmd`s, so routing one would hang until the
// relay timed out into a 504. We answer 501 with something actionable instead.

/// Resolve + authorize the target agent for an update op. `Err` is the ready-made
/// HTTP response (404 unknown/offline, 403 not the owner, 501 agent too old).
fn update_target(
    hub: &HubState,
    scope: &Visibility,
    machine: &str,
) -> Result<std::sync::Arc<crate::registry::AgentConn>, Response> {
    update_target_caps(hub, scope, machine, false)
}

/// As `update_target`, but additionally requires `assistant-install` when the
/// request asks the agent to **install** something (proposal 0050 D1). Installing
/// is not more permission than updating — it's the same owner-only gate — but it
/// IS a newer agent capability, and the field that carries it is additive, so
/// without this check a 0049-era agent would quietly run an update-only job and
/// the user would believe a CLI had been installed.
fn update_target_caps(
    hub: &HubState,
    scope: &Visibility,
    machine: &str,
    needs_install: bool,
) -> Result<std::sync::Arc<crate::registry::AgentConn>, Response> {
    let agent = hub
        .registry
        .resolve_scoped(scope, machine, None)
        .ok_or_else(|| (StatusCode::NOT_FOUND, UNRESOLVED).into_response())?;
    if !scope.owns_agent(&agent) {
        return Err((
            StatusCode::FORBIDDEN,
            "only the machine's owner can update its coding assistants",
        )
            .into_response());
    }
    if !agent.caps.iter().any(|c| c == cc_screen_protocol::hub::CAP_ASSISTANT_UPDATE) {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "this machine's agent is too old to self-update — run `cc-screen-rust update` on it",
        )
            .into_response());
    }
    if needs_install
        && !agent.caps.iter().any(|c| c == cc_screen_protocol::hub::CAP_ASSISTANT_INSTALL)
    {
        return Err((
            StatusCode::NOT_IMPLEMENTED,
            "this machine's agent is too old to install assistants — run `cc-screen-rust update` on it",
        )
            .into_response());
    }
    Ok(agent)
}

/// Send an update op to an authorized agent and map its reply. A `409` carries
/// the *running* job as its body (the agent JSON-encodes it into `msg`), so the
/// client can switch to watching that job instead of racing a second one.
async fn update_send(agent: std::sync::Arc<crate::registry::AgentConn>, cmd: Cmd) -> Response {
    match agent.request(cmd).await {
        Ok(CmdResult::Json(v)) => Json(v).into_response(),
        Ok(CmdResult::Error { code, msg }) => {
            let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST);
            match serde_json::from_str::<Value>(&msg) {
                Ok(v) => (status, Json(v)).into_response(),
                Err(_) => (status, msg).into_response(),
            }
        }
        Ok(_) => (StatusCode::INTERNAL_SERVER_ERROR, "unexpected agent reply").into_response(),
        Err(RequestErr::Offline) => (StatusCode::SERVICE_UNAVAILABLE, "machine offline").into_response(),
        Err(RequestErr::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "agent did not respond").into_response(),
    }
}

/// The PWA sends `installMissing`, so this MUST be `camelCase` like `UpdateReq`
/// — today's single-word fields hide the mismatch, and a silently-dropped
/// `install_missing` would be the exact "believed it installed" failure the
/// capability gate exists to prevent (proposal 0050 D4).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateBody {
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    restart: Option<String>,
    #[serde(default)]
    install_missing: bool,
}

pub async fn update_assistants(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    body: Option<Json<UpdateBody>>,
) -> Response {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let agent = match update_target_caps(&hub, &scope, &q.machine, b.install_missing) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let cmd = Cmd::UpdateAssistants {
        tools: b.tools,
        restart: b.restart.unwrap_or_else(|| "updated".into()),
        install_missing: b.install_missing,
    };
    update_send(agent, cmd).await
}

/// `GET /api/assistants/plan` — relayed straight through. Same owner-only gate:
/// the plan names this machine's install commands, which is administrative
/// detail about someone's host.
pub async fn install_plan(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
) -> Response {
    match update_target_caps(&hub, &scope, &q.machine, true) {
        Ok(agent) => update_send(agent, Cmd::InstallPlan { tools: Vec::new() }).await,
        Err(resp) => resp,
    }
}

pub async fn update_status(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
) -> Response {
    match update_target(&hub, &scope, &q.machine) {
        Ok(agent) => update_send(agent, Cmd::UpdateStatus).await,
        Err(resp) => resp,
    }
}

// ── Favorites (hub-local; per-tenant in multi-tenant mode) ────────────────────
// 0042 candidate finding #1: the historical single `config_dir/favorites.json`
// is correct for a one-operator fleet but a cross-tenant leak on a multi-tenant
// hub. So the file is keyed by the caller's scope: single-tenant
// (`Visibility::All`) keeps `favorites.json` byte-for-byte; a multi-tenant
// caller gets `config_dir/favorites/<user_id>.json`. A pre-existing shared
// favorites.json on a multi-tenant hub is deliberately NOT migrated — it is a
// mixed-tenant file, so every user starts empty and the old file is simply
// never read in multi-tenant mode.
fn favorites_path_in(config_dir: &std::path::Path, scope: &Visibility) -> Option<std::path::PathBuf> {
    match scope {
        Visibility::All => Some(config_dir.join("favorites.json")),
        Visibility::User(v) => {
            let uid = v.user_id.as_str();
            // user_id is an opaque token-safe id, but never trust it as a path
            // component: [A-Za-z0-9_-] only (rejects separators, dots, empty).
            if uid.is_empty()
                || !uid.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return None;
            }
            Some(config_dir.join("favorites").join(format!("{uid}.json")))
        }
    }
}

fn favorites_path(hub: &HubState, scope: &Visibility) -> Option<std::path::PathBuf> {
    favorites_path_in(&hub.config_dir, scope)
}

pub async fn get_favorites(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
) -> Json<Vec<Favorite>> {
    let list = favorites_path(&hub, &scope)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Vec<Favorite>>(&s).ok())
        .unwrap_or_default();
    Json(list)
}

pub async fn put_favorites(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
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
    // A gated route always carries a real scope; a missing/unsanitizable tenant
    // id fails closed rather than falling back to a shared file.
    let Some(path) = favorites_path(&hub, &scope) else {
        return (StatusCode::FORBIDDEN, "no tenant for favorites").into_response();
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
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
    scope: &Visibility,
    machine: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
) -> Response {
    // The file surface resolves with `resolve_browsable`, not `resolve_scoped`:
    // any grant on the agent (agent / session / team) reaches it, because every
    // one of them already carries attach — and attach is keyboard input into an
    // assistant running with `--dangerously-skip-permissions`, which can read and
    // write the same files. See `Visibility::may_browse_agent`. Lifecycle ops
    // above still go through `route`/`may_use_agent`.
    let cmd = Cmd::File { op: op.to_string(), args };
    let routed = match hub.registry.resolve_browsable(scope, machine, session) {
        Some(agent) => agent.request(cmd).await.map_err(|e| match e {
            RequestErr::Offline => (StatusCode::SERVICE_UNAVAILABLE, "machine offline").into_response(),
            RequestErr::Timeout => (StatusCode::GATEWAY_TIMEOUT, "agent did not respond").into_response(),
        }),
        None => Err((StatusCode::NOT_FOUND, UNRESOLVED).into_response()),
    };
    match routed {
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

pub async fn dirs(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &scope, &q.machine, opt(&q.session), "dirs", json!({ "path": q.path, "session": q.session })).await
}

pub async fn files(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<FileGetQ>) -> Response {
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

pub async fn dirs_search(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(qy): Query<DirSearchQ>) -> Response {
    file_route(&hub, &scope, &qy.machine, opt(&qy.session), "dirs_search", json!({ "q": qy.q, "root": qy.root })).await
}

// Recursive fuzzy *file* search (proposal 0027), per-agent like `dirs_search`:
// the chosen machine searches its own $HOME. `?session=` both disambiguates the
// owning agent and lets that agent default the root to the session's project.
pub async fn files_search(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(qy): Query<DirSearchQ>) -> Response {
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

pub async fn file_read(State(hub): State<HubState>, Extension(scope): Extension<Visibility>, Query(q): Query<FileGetQ>) -> Response {
    file_route(&hub, &scope, &q.machine, opt(&q.session), "read", json!({ "path": q.path })).await
}

// POST handlers forward the client's JSON body straight through as the op args;
// path-only, so they route to the explicit (or single online) machine.
pub async fn file_write(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "write", body).await
}

pub async fn file_delete(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "delete", body).await
}

pub async fn mkdir(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "mkdir", body).await
}

pub async fn rmdir(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "rmdir", body).await
}

pub async fn rename(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
    Query(q): Query<MachineQ>,
    Json(body): Json<Value>,
) -> Response {
    file_route(&hub, &scope, &q.machine, None, "rename", body).await
}

pub async fn move_path(
    State(hub): State<HubState>,
    Extension(scope): Extension<Visibility>,
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
        // Device-flow host endpoints are unauthenticated at the cookie gate (the
        // device_code / uplink token IS the bearer); /api/device/validate authns
        // via the uplink token in its own handler (proposal 0048). /api/device/approve
        // is intentionally NOT exempt — it needs the user's session to bind the
        // enrollment to their tenant.
        || matches!(path.as_str(), "/api/device/code" | "/api/device/token" | "/api/device/validate");
    // The invite-link read (proposal 0056 C4) is public by design: the token is
    // the capability, and the landing page must render before login. The route
    // only exists in the multi-tenant build, so the exemption is gated with it.
    // The 0060 client device-flow endpoints are unauthenticated like their agent
    // siblings (the device_code is the bearer); same multi-tenant-only gating.
    #[cfg(feature = "multi-tenant")]
    let exempt = exempt
        || path.starts_with("/api/invite/")
        // The org-invite landing (proposal 0063 B2) is public like its 0056
        // sibling: the token is the capability, the page renders before login.
        || path.starts_with("/api/org-invite/")
        // The read-only link grant (proposal 0083 Part C): the token IS the
        // capability and its whole purpose is a reader with no account. The
        // handlers resolve it against their own table — never `Visibility`, and
        // never `file_route` — and can only read; see `link.rs`.
        || path.starts_with("/api/link/")
        || matches!(path.as_str(), "/api/device/client/code" | "/api/device/client/token");
    // The Stripe webhook (proposal 0058 B2) authenticates via its Stripe-Signature
    // HMAC (verified in the handler), not a session cookie — and Stripe sends no
    // Origin header, so the origin check above passes it through.
    #[cfg(feature = "multi-tenant")]
    let exempt = exempt || path == "/api/billing/webhook";

    // Multi-tenant (proposal 0001 §4.1): identity comes from the session cookie,
    // not the shared secret. A gated request without a valid session is refused
    // here; the resolved tenant scope is stashed for the handlers so every relay
    // lookup is filtered to the caller's own agents.
    if hub.multi_tenant() {
        #[allow(unused_mut)]
        let mut user = hub.client_auth.user_from_cookie(req.headers());
        // Proposal 0060 B3: a headless client (`ccs`) authenticates with a
        // per-user client token instead of a cookie — resolve `Bearer → user`
        // against `client_tokens` (and ONLY `client_tokens`: agent uplink
        // tokens must never authenticate a client, and vice versa — the 0001
        // two-credential invariant). Everything downstream (Visibility scope,
        // sharing, WS attach, bulk) inherits with no handler changes.
        #[cfg(feature = "multi-tenant")]
        if user.is_none() && !exempt {
            if let Some(bearer) = cc_screen_auth::bearer_token(req.headers()) {
                user = hub.user_by_client_token_hash(&cc_screen_auth::sha256_hex(bearer)).await;
            }
        }
        if !exempt && user.is_none() {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
        // Resolve the caller's full visibility — ownership + sharing grants
        // (proposal 0039) — in one DB load, here next to where identity is derived.
        // Exempt paths with no session get an empty-user scope that matches no agent
        // (harmless — they don't consult it); gated paths always have a real user.
        let scope = match user {
            Some(uid) => hub.visibility_for(&uid).await,
            None => Visibility::user(String::new()),
        };
        req.extensions_mut().insert(scope);
        return next.run(req).await;
    }

    // Single-tenant: every authed client sees every agent (today's behavior).
    req.extensions_mut().insert(Visibility::All);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // 0042 candidate finding #1: favorites are per-tenant in multi-tenant mode,
    // and the single-tenant path is byte-for-byte the historical one.
    #[test]
    fn favorites_path_is_scope_keyed() {
        let dir = Path::new("/cfg");
        // Single-tenant: the historical shared file, unchanged.
        assert_eq!(
            favorites_path_in(dir, &Visibility::All),
            Some(dir.join("favorites.json")),
        );
        // Multi-tenant: per-user file under favorites/, keyed by the (opaque,
        // token-safe) user id.
        assert_eq!(
            favorites_path_in(dir, &Visibility::user("u_AbC-123")),
            Some(dir.join("favorites").join("u_AbC-123.json")),
        );
        // Two tenants never share a path.
        assert_ne!(
            favorites_path_in(dir, &Visibility::user("alice")),
            favorites_path_in(dir, &Visibility::user("bob")),
        );
        // And no tenant path is the single-tenant shared file (no read of the
        // legacy mixed-tenant favorites.json in multi-tenant mode).
        assert_ne!(
            favorites_path_in(dir, &Visibility::user("alice")),
            favorites_path_in(dir, &Visibility::All),
        );
    }

    // The user id is server-minted, but never trust it as a path component:
    // anything outside [A-Za-z0-9_-] (and the empty exempt-path scope) fails
    // closed instead of escaping config_dir or falling back to a shared file.
    #[test]
    fn favorites_path_rejects_unsafe_user_ids() {
        let dir = Path::new("/cfg");
        for bad in ["", "../evil", "a/b", "a\\b", "a.b", "..", "a b", "a\0b"] {
            assert_eq!(favorites_path_in(dir, &Visibility::user(bad)), None, "must refuse {bad:?}");
        }
    }
}
