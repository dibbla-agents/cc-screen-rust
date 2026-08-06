//! Org membership lifecycle endpoints (proposal 0063), compiled only under
//! `multi-tenant`. Modeled line-for-line on `share.rs`: every handler re-derives
//! the actor from the session cookie and resolves *their* org from membership —
//! no `:org_id` in paths, so nobody can name someone else's org (a whole class
//! of confused-deputy bugs is unrepresentable). Non-members get **404** on every
//! org route (existence isn't disclosed, 0040 §4); members whose role forbids an
//! action get **403** (the org is already disclosed to them, B3).
//!
//! Audit action vocabulary (0063 Part D — dotted, closed set):
//!   org.created            member.joined          member.left
//!   member.removed         member.role_changed    invite.created
//!   invite.declined        invite.revoked         invite.expired   (sweep; actor NULL)
//!   machine.visibility_changed                    org.seats_changed
//!   share.created          share.revoked          (actor is an org member; share.rs)
//!   billing.checkout       billing.subscription   (0064's webhook/checkout path)

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{OrgInviteRow, OrgRow, ShareOutcome, Store};
use crate::state::HubState;

/// The normative consent copy (proposal 0063 B2) — part of the consent model,
/// not decoration: rendered on the invite landing and in the inbox DTO so the
/// accept is informed. The acceptance criteria grep for it.
pub const CONSENT_COPY: &str = "Joining makes your machines on cc-screen visible to this team \
(view-only). You can hide any machine in settings.";

/// The public base for org-invite links — `share::invite_url`'s twin on the
/// `/org-invite/<token>` path.
fn org_invite_url(token: &str) -> String {
    let base = std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    format!("{base}/org-invite/{token}")
}

/// Resolve the acting user + their org membership, mapping the failure modes to
/// the B1 statuses: no cookie → 401, no org → 404 (never disclosed).
async fn actor_org(
    hub: &HubState,
    headers: &HeaderMap,
) -> Result<(Arc<dyn Store>, String, OrgRow, String), Response> {
    let store = hub
        .store()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response())?;
    let Some(actor) = hub.client_auth.user_from_cookie(headers) else {
        return Err((StatusCode::UNAUTHORIZED, "login required").into_response());
    };
    let Some((org, role)) = store.org_for_user(&actor).await else {
        return Err((StatusCode::NOT_FOUND, "no team").into_response());
    };
    Ok((store, actor, org, role))
}

/// The actor without requiring membership (create / inbox / accept paths).
fn actor_only(hub: &HubState, headers: &HeaderMap) -> Result<(Arc<dyn Store>, String), Response> {
    let store = hub
        .store()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response())?;
    let Some(actor) = hub.client_auth.user_from_cookie(headers) else {
        return Err((StatusCode::UNAUTHORIZED, "login required").into_response());
    };
    Ok((store, actor))
}

fn is_admin(role: &str) -> bool {
    matches!(role, "owner" | "admin")
}

/// Map the store's prefixed error vocabulary to HTTP: `SEATS:` → 402 (the plan-
/// limit convention), `ONEORG:`/`OWNER:` → 409, anything else → 400.
fn store_err(e: anyhow::Error) -> Response {
    let msg = e.to_string();
    if let Some(m) = msg.strip_prefix("SEATS:") {
        return (StatusCode::PAYMENT_REQUIRED, m.to_string()).into_response();
    }
    if let Some(m) = msg.strip_prefix("ONEORG:") {
        return (StatusCode::CONFLICT, m.to_string()).into_response();
    }
    if let Some(m) = msg.strip_prefix("OWNER:") {
        return (StatusCode::CONFLICT, m.to_string()).into_response();
    }
    (StatusCode::BAD_REQUEST, msg).into_response()
}

#[derive(Deserialize)]
pub struct CreateOrgReq {
    name: String,
}

/// `POST /api/orgs` — create a team; the caller becomes its owner. The org is
/// dormant (`seats: 0`) until 0064's checkout or the `org seats` CLI activates
/// it — creating one can never reduce anyone's entitlements (0063 C1).
pub async fn create(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<CreateOrgReq>) -> Response {
    let (store, actor) = match actor_only(&hub, &headers) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match store.org_create(&actor, &req.name).await {
        Ok(id) => (StatusCode::OK, Json(json!({ "id": id }))).into_response(),
        Err(e) => store_err(e),
    }
}

/// `GET /api/orgs/mine` — my org: the TeamCard's whole data contract. 404 when
/// the caller is in no org.
pub async fn mine(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let members = store.org_members(&org.id).await;
    let member_views: Vec<Value> = members
        .iter()
        .map(|m| json!({ "userId": m.user_id, "email": m.email, "role": m.role, "joinedAt": m.created_at }))
        .collect();
    // Pending invites are management surface — owner/admin only (B3).
    let invites: Vec<Value> = if is_admin(&role) {
        store
            .org_invite_outbox(&org.id)
            .await
            .into_iter()
            .filter(|i| i.status == "pending")
            .map(|i| invite_view(&i, true))
            .collect()
    } else {
        Vec::new()
    };
    // My machines with the per-machine opt-out flag (the consent surface).
    let machines: Vec<Value> = store
        .list_agents(&actor)
        .await
        .into_iter()
        .map(|a| json!({ "agentId": a.agent_id, "machine": a.machine_id, "teamVisible": a.team_visible }))
        .collect();
    Json(json!({
        "org": {
            "id": org.id,
            "name": org.name,
            "seats": org.seat_count,
            "memberCount": members.len(),
            "planStatus": org.plan_status,
            "periodEnd": org.current_period_end,
        },
        "myRole": role,
        "members": member_views,
        "invites": invites,
        "machines": machines,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct OrgInviteReq {
    email: String,
    #[serde(default)]
    role: Option<String>,
}

/// `POST /api/orgs/invites` — owner/admin invite by email. Both arms (account /
/// no account yet) answer the same `{id, status, invite_url}` shape — the 0042
/// account-existence-oracle fix, carried over from `share::create`.
pub async fn invite_create(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<OrgInviteReq>) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_admin(&role) {
        return (StatusCode::FORBIDDEN, "only a team owner or admin can invite").into_response();
    }
    let invite_role = req.role.as_deref().unwrap_or("member");
    let (id, status, token) = match store.org_invite_create(&org.id, &actor, &req.email, invite_role).await {
        Ok(v) => v,
        Err(e) => return store_err(e),
    };
    store
        .audit_append(
            &org.id,
            Some(&actor),
            "invite.created",
            Some(req.email.trim()),
            Some(&format!("{{\"role\":\"{invite_role}\"}}")),
        )
        .await;
    // Best-effort push when the address already has an account (the copyable
    // link is the channel otherwise — the hub sends no mail, 0058's ops fact).
    if let Some(grantee) = hub.user_id_by_email(&req.email).await {
        let inviter_email = hub.user_email(&actor).await.unwrap_or_default();
        hub.push
            .notify_scoped(
                Some(&grantee),
                "Team invitation",
                &format!("{inviter_email} invited you to join {}", org.name),
                "org-invite",
            )
            .await;
    }
    (StatusCode::OK, Json(json!({ "id": id, "status": status, "invite_url": org_invite_url(&token) })))
        .into_response()
}

/// `GET /api/orgs/invites` — owner/admin: the outbox (all statuses).
pub async fn invite_outbox(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let (store, _actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_admin(&role) {
        return (StatusCode::FORBIDDEN, "only a team owner or admin can manage invites").into_response();
    }
    let out: Vec<Value> = store.org_invite_outbox(&org.id).await.iter().map(|i| invite_view(i, true)).collect();
    Json(out).into_response()
}

/// `GET /api/orgs/invites/inbox` — my pending org invites (by my account email).
/// Deliberately membership-free: the whole point is that the caller is NOT in
/// the org yet. Each row carries the consent copy — the accept must be informed.
pub async fn invite_inbox(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let (store, actor) = match actor_only(&hub, &headers) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(email) = store.user_email(&actor).await else {
        return Json(Vec::<Value>::new()).into_response();
    };
    let mut out = Vec::new();
    for inv in store.org_invite_inbox(&email).await {
        let mut v = invite_view(&inv, false);
        if let Some(org) = store.org_get(&inv.org_id).await {
            v["orgName"] = json!(org.name);
        }
        if let Some(m) = v.as_object_mut() {
            m.insert("inviterEmail".into(), json!(store.user_email(&inv.inviter_user_id).await));
            m.insert("consent".into(), json!(CONSENT_COPY));
        }
        out.push(v);
    }
    Json(out).into_response()
}

/// `POST /api/orgs/invites/:id/accept` — the invitee joins (seat gate → 402,
/// one-org → 409). Team visibility materializes inside the store transition.
pub async fn invite_accept(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    invite_respond(&hub, &headers, &id, true).await
}

/// `POST /api/orgs/invites/:id/decline`.
pub async fn invite_decline(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    invite_respond(&hub, &headers, &id, false).await
}

async fn invite_respond(hub: &HubState, headers: &HeaderMap, id: &str, accept: bool) -> Response {
    let (store, actor) = match actor_only(hub, headers) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match store.org_invite_respond(&actor, id, accept).await {
        Ok(outcome) => outcome_to_response(outcome),
        Err(e) => store_err(e),
    }
}

/// `POST /api/orgs/invites/:id/revoke` — owner/admin cancels a pending invite.
pub async fn invite_revoke(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_admin(&role) {
        return (StatusCode::FORBIDDEN, "only a team owner or admin can revoke invites").into_response();
    }
    let outcome = store.org_invite_revoke(&org.id, &id).await;
    if matches!(outcome, ShareOutcome::Ok(ref s) if s == "revoked") {
        store.audit_append(&org.id, Some(&actor), "invite.revoked", Some(&id), None).await;
    }
    outcome_to_response(outcome)
}

/// `GET /api/org-invite/:token` — the invite-link read, beside `/api/invite/:token`
/// with the same posture: unauthenticated (the token IS the capability), throttled
/// per source, 404 for dead/unknown. The consent copy is part of the payload —
/// the landing must state what joining means before the accept.
pub async fn invite_info(State(hub): State<HubState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response();
    };
    let source = format!("org-invite:{}", cc_screen_auth::source_key(&headers));
    let now = std::time::Instant::now();
    if hub.login_throttle.locked_for(&source, now).is_some() {
        return (StatusCode::TOO_MANY_REQUESTS, "too many attempts").into_response();
    }
    let Some(row) = store.org_invite_by_token(&token).await else {
        hub.login_throttle.record_failure(&source, now);
        return (StatusCode::NOT_FOUND, "no such invitation").into_response();
    };
    let inviter_email = store.user_email(&row.inviter_user_id).await.unwrap_or_default();
    let org_name = store.org_get(&row.org_id).await.map(|o| o.name).unwrap_or_default();
    Json(json!({
        "kind": "team",
        "email": row.email,
        "inviterEmail": inviter_email,
        "orgName": org_name,
        "role": row.role,
        "consent": CONSENT_COPY,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct RoleReq {
    role: String,
}

/// `POST /api/orgs/members/:user_id/role` — owner only; `role: "owner"` is an
/// atomic ownership transfer (exactly one owner at all times).
pub async fn member_role(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(req): Json<RoleReq>,
) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if role != "owner" {
        return (StatusCode::FORBIDDEN, "only the team owner can change roles").into_response();
    }
    match store.org_set_role(&org.id, &user_id, &req.role).await {
        Ok(()) => {
            store
                .audit_append(
                    &org.id,
                    Some(&actor),
                    "member.role_changed",
                    Some(&user_id),
                    Some(&format!("{{\"role\":\"{}\"}}", req.role)),
                )
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => store_err(e),
    }
}

/// `POST /api/orgs/members/:user_id/remove` — owner/admin; an admin cannot
/// remove the owner or a fellow admin (B3). Prunes the member's team shares.
pub async fn member_remove(State(hub): State<HubState>, headers: HeaderMap, Path(user_id): Path<String>) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_admin(&role) {
        return (StatusCode::FORBIDDEN, "only a team owner or admin can remove members").into_response();
    }
    let members = store.org_members(&org.id).await;
    let Some(target) = members.iter().find(|m| m.user_id == user_id) else {
        return (StatusCode::NOT_FOUND, "no such member").into_response();
    };
    if role == "admin" && target.role != "member" {
        return (StatusCode::FORBIDDEN, "an admin can only remove members").into_response();
    }
    match store.org_remove_member(&org.id, &user_id).await {
        Ok(()) => {
            store
                .audit_append(&org.id, Some(&actor), "member.removed", Some(&target.email), None)
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => store_err(e),
    }
}

/// `POST /api/orgs/leave` — member/admin leaves; the owner must transfer first
/// (409). Team visibility is pruned both directions by the removal.
pub async fn leave(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let (store, actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if role == "owner" {
        return (StatusCode::CONFLICT, "transfer ownership before leaving the team").into_response();
    }
    let email = store.user_email(&actor).await.unwrap_or_default();
    match store.org_remove_member(&org.id, &actor).await {
        Ok(()) => {
            store.audit_append(&org.id, Some(&actor), "member.left", Some(&email), None).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => store_err(e),
    }
}

#[derive(Deserialize)]
pub struct VisibilityReq {
    visible: bool,
}

/// `POST /api/orgs/machines/:agent_id/visibility` — the consent model's opt-out
/// (0063 §consent). Belongs to the MACHINE OWNER alone, whatever their role —
/// no admin override. Takes effect within the action (materialize/prune), not
/// the nightly pass.
pub async fn machine_visibility(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(req): Json<VisibilityReq>,
) -> Response {
    let (store, actor, org, _role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !store.set_team_visible(&actor, &agent_id, req.visible).await {
        return (StatusCode::NOT_FOUND, "no such machine you own").into_response();
    }
    if req.visible {
        store.team_shares_materialize(&org.id).await;
    } else {
        store.team_shares_prune(&org.id).await;
    }
    let label = store
        .list_agents(&actor)
        .await
        .into_iter()
        .find(|a| a.agent_id == agent_id)
        .map(|a| a.machine_id)
        .unwrap_or_default();
    store
        .audit_append(
            &org.id,
            Some(&actor),
            "machine.visibility_changed",
            Some(&agent_id),
            Some(&format!("{{\"machine\":{},\"visible\":{}}}", serde_json::to_string(&label).unwrap_or_default(), req.visible)),
        )
        .await;
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub struct AuditQ {
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /api/orgs/audit?before=<id>&limit=50` — owner/admin (B3). Keyset-paged
/// on the AUTOINCREMENT id, newest first; actor ids resolved to emails.
pub async fn audit(State(hub): State<HubState>, headers: HeaderMap, Query(q): Query<AuditQ>) -> Response {
    let (store, _actor, org, role) = match actor_org(&hub, &headers).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !is_admin(&role) {
        // A member is *told* the log exists (B3 discloses the org to them), but
        // the endpoint stays 404-shaped per the spec's member acceptance.
        return (StatusCode::NOT_FOUND, "no team audit log").into_response();
    }
    let mut out = Vec::new();
    for row in store.audit_page(&org.id, q.before, q.limit.unwrap_or(50)).await {
        let actor_email = match &row.actor_user_id {
            Some(uid) => store.user_email(uid).await,
            None => None,
        };
        out.push(json!({
            "id": row.id,
            "at": row.at,
            "actorEmail": actor_email,
            "action": row.action,
            "target": row.target,
            "detail": row.detail,
        }));
    }
    Json(out).into_response()
}

fn outcome_to_response(outcome: ShareOutcome) -> Response {
    match outcome {
        ShareOutcome::Ok(status) => (StatusCode::OK, Json(json!({ "status": status }))).into_response(),
        ShareOutcome::Conflict => (StatusCode::CONFLICT, "this invite is no longer pending").into_response(),
        ShareOutcome::NotFound => (StatusCode::NOT_FOUND, "no such invite").into_response(),
    }
}

/// The org-invite DTO. `admin` includes the link (management surface); the
/// inbox variant omits it (the invitee acts by id, not token).
fn invite_view(row: &OrgInviteRow, admin: bool) -> Value {
    let mut v = json!({
        "id": row.id,
        "email": row.email,
        "role": row.role,
        "status": row.status,
        "createdAt": row.created_at,
        "expiresAt": row.expires_at,
    });
    if admin {
        v["inviteUrl"] = json!(org_invite_url(&row.token));
    }
    v
}
