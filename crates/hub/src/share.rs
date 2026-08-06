//! Share-invite lifecycle endpoints (proposal 0040), compiled only under
//! `multi-tenant`. An owner invites another user to one of their agents or
//! sessions; the recipient sees it in their **inbox** and explicitly accepts or
//! declines. An accepted invite materialises the durable 0039 grant the
//! visibility predicate reads (`db::share_respond`); revoke/decline/expiry strip
//! it. Notification rides the existing per-tenant Web Push — no new channel, no
//! agent-protocol change.
//!
//! Every handler re-derives the actor from the session cookie (the same pattern
//! as `device::approve` / `account::list`) and is owner/grantee-scoped, so one
//! tenant can never see or drive another's invites.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db::{ShareInviteRow, ShareOutcome};
use crate::registry::Visibility;
use crate::state::HubState;

#[derive(Deserialize)]
pub struct CreateShareReq {
    /// The recipient, named by account email.
    grantee_email: String,
    /// The machine label to share (resolved under the inviter's own scope).
    #[serde(default)]
    machine: String,
    /// A session name → a session invite; omitted/empty → an agent-wide invite.
    #[serde(default)]
    session: Option<String>,
    /// Owner-peek (agent invites only): keep sight of the grantee's sessions.
    #[serde(default)]
    owner_peek: bool,
}

/// The public base for invite links: `CCHUB_PUBLIC_URL` when configured (the
/// same env the OAuth redirect + device verification_uri build on), else a
/// relative path the frontend resolves against its own origin.
fn invite_url(token: &str) -> String {
    let base = std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    format!("{base}/invite/{token}")
}

/// `POST /api/shares` — create (or re-offer) an invite to a user by email.
///
/// Proposal 0056 C2: the two arms — the email has an account ([0040]'s invite)
/// vs. it doesn't (an `email_invites` row that converts on signup) — return the
/// SAME status and body shape (`{id, status:"pending", invite_url}`), so share
/// creation no longer discloses account existence ([0042]). Both arms mint a
/// link token in `email_invites` (the known-user row is pre-converted, so it
/// never re-attaches — it only backs the link).
pub async fn create(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<CreateShareReq>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    // Plan gate (proposal 0058 A3): creating shares/invites is a paid capability;
    // receiving is not — inbox/accept/decline/received/leave/revoke and the
    // invite-link read stay ungated. 402 mirrors the session-cap gate; no "LIMIT:"
    // prefix (that only exists to cross the Store trait's anyhow boundary).
    if !hub.limits_for(&actor).await.can_create_shares {
        return (
            StatusCode::PAYMENT_REQUIRED,
            "Sharing your machines is a Pro feature. You can still accept shares others send you."
                .to_string(),
        )
            .into_response();
    }
    let session = req.session.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let kind = if session.is_some() { "session" } else { "agent" };

    // Resolve the machine under the inviter's OWN scope (no grants) — you can only
    // invite to a resource you own (proposal 0040 §5, the §4.1 isolation keystone).
    // Done BEFORE the grantee lookup so both arms share every early exit.
    let owner_scope = Visibility::user(actor.clone());
    let Some(agent) = hub.registry.resolve_scoped(&owner_scope, &req.machine, session) else {
        return (StatusCode::NOT_FOUND, "no such machine/session you own (try ?machine=)").into_response();
    };
    let agent_id = agent.agent_id.clone();
    let machine_label = agent.machine_id.clone();

    let (id, status, token) = match hub.user_id_by_email(&req.grantee_email).await {
        Some(grantee) => {
            if grantee == actor {
                return (StatusCode::BAD_REQUEST, "cannot share with yourself").into_response();
            }
            let (id, status) =
                match hub.share_create(&actor, &grantee, kind, &agent_id, session, req.owner_peek).await {
                    Ok(v) => v,
                    Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
                };
            // Best-effort push, scoped to the grantee's own devices (§8). A missed
            // buzz never loses the invite — it sits in /inbox.
            let inviter_email = hub.user_email(&actor).await.unwrap_or_default();
            let what = match session {
                Some(s) => format!("session {s} on {machine_label}"),
                None => format!("machine {machine_label}"),
            };
            hub.push
                .notify_scoped(
                    Some(&grantee),
                    "New share invitation",
                    &format!("{inviter_email} shared {what} with you"),
                    "share-invite",
                )
                .await;
            // Mint the link token (pre-converted — it identifies, never grants).
            let token = hub
                .email_invite_create(&actor, &req.grantee_email, kind, &agent_id, session, req.owner_peek, true)
                .await
                .map(|(_, t)| t)
                .unwrap_or_default();
            (id, status, token)
        }
        None => {
            match hub
                .email_invite_create(&actor, &req.grantee_email, kind, &agent_id, session, req.owner_peek, false)
                .await
            {
                Ok((id, token)) => (id, "pending".to_string(), token),
                Err(e) => {
                    let msg = e.to_string();
                    // The per-inviter live cap (C2's abuse bound) → 429.
                    return match msg.strip_prefix("CAP:") {
                        Some(m) => (StatusCode::TOO_MANY_REQUESTS, m.to_string()).into_response(),
                        None => (StatusCode::BAD_REQUEST, msg).into_response(),
                    };
                }
            }
        }
    };
    // Team-history audit (0063 D1): appended ONLY when the actor is an org
    // member — a team's share activity is org-visible history; a lone user's
    // sharing stays unlogged, exactly today's behavior.
    if let Some(store) = hub.store() {
        if let Some((org, _)) = store.org_for_user(&actor).await {
            let detail = format!(
                "{{\"machine\":{},\"kind\":\"{kind}\"}}",
                serde_json::to_string(&machine_label).unwrap_or_default()
            );
            store
                .audit_append(&org.id, Some(&actor), "share.created", Some(req.grantee_email.trim()), Some(&detail))
                .await;
        }
    }
    (StatusCode::OK, Json(json!({ "id": id, "status": status, "invite_url": invite_url(&token) })))
        .into_response()
}

/// `GET /api/invite/:token` — resolve an invite link (unauthenticated; the token
/// IS the read capability — it was handed to the invited address). Discloses the
/// invited email + inviter to the token holder only; a dead/unknown token 404s.
/// Throttled per source against token brute force. If the caller is already
/// signed in with the MATCHING email, defensively attach (proposal 0056 C3.3).
pub async fn invite_info(State(hub): State<HubState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let source = format!("invite:{}", cc_screen_auth::source_key(&headers));
    let now = std::time::Instant::now();
    if hub.login_throttle.locked_for(&source, now).is_some() {
        return (StatusCode::TOO_MANY_REQUESTS, "too many attempts").into_response();
    }
    let Some(row) = hub.email_invite_by_token(&token).await else {
        hub.login_throttle.record_failure(&source, now);
        return (StatusCode::NOT_FOUND, "no such invitation").into_response();
    };
    let inviter_email = hub.user_email(&row.inviter_user_id).await.unwrap_or_default();
    if let Some(uid) = hub.client_auth.user_from_cookie(&headers) {
        if hub.user_email(&uid).await.as_deref() == Some(row.email.as_str()) {
            hub.attach_email_invites(&uid, &row.email).await;
        }
    }
    Json(json!({ "email": row.email, "inviterEmail": inviter_email, "kind": row.resource_kind }))
        .into_response()
}

/// `GET /api/shares/inbox` — the caller's pending, unexpired invites.
pub async fn inbox(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let mut out = Vec::new();
    for row in hub.share_inbox(&actor).await {
        if session_dead(&hub, &row) {
            continue; // the underlying session ended — don't show a dead offer
        }
        out.push(invite_view(&hub, &row).await);
    }
    Json(out).into_response()
}

/// `GET /api/shares/outbox` — the invites the caller has sent (all statuses),
/// plus their live email invites (proposal 0056 Part C) with status `"invited"`
/// and the invited address as the counterpart label.
pub async fn outbox(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let mut out = Vec::new();
    for row in hub.share_outbox(&actor).await {
        out.push(invite_view(&hub, &row).await);
    }
    let inviter_email = hub.user_email(&actor).await;
    for row in hub.email_invite_outbox(&actor).await {
        let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone());
        let permission = if row.resource_kind == "session" { "view" } else { "use" };
        out.push(json!({
            "id": row.id,
            "inviterEmail": inviter_email,
            "granteeEmail": row.email,
            "resourceKind": row.resource_kind,
            "agentId": row.agent_id,
            "machine": machine,
            "session": row.session_name,
            "permission": permission,
            "ownerPeek": row.owner_peek,
            "status": "invited",
            "createdAt": row.created_at,
            "expiresAt": row.expires_at,
            "inviteUrl": invite_url(&row.token),
        }));
    }
    Json(out).into_response()
}

/// `GET /api/shares/received` — the active shares granted *to* the caller (the
/// "shared with you" list + the feed for the shared-vs-owned badge, proposal
/// 0041). Each row carries the machine label and the owner's email for display.
pub async fn received(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let mut out = Vec::new();
    for row in hub.shares_to_me(&actor).await {
        // Lazy session-liveness: hide a session grant whose session has ended on a
        // currently-connected agent (best-effort, §6 of 0040). Team rows are
        // agent-scoped, so the filter doesn't apply to them (0065 A4).
        if row.kind == "session" {
            if let (Some(agent), Some(name)) = (hub.registry.get(&row.agent_id), row.session.as_deref()) {
                if !agent.sessions_tagged().iter().any(|s| s.name == name) {
                    continue;
                }
            }
        }
        let owner_email = hub.user_email(&row.owner_user_id).await;
        let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone());
        // Team-materialized rows (0065 A4): view-level, badged by origin, with
        // the implying org's name for the tooltip.
        let team = row.kind == "team";
        let org_name = match (team, row.org_id.as_deref(), hub.store()) {
            (true, Some(oid), Some(store)) => store.org_get(oid).await.map(|o| o.name),
            _ => None,
        };
        out.push(json!({
            "id": row.id,
            "agentId": row.agent_id,
            "machine": machine,
            "session": row.session,
            "kind": row.kind,
            "permission": if row.kind == "agent" { "use" } else { "view" },
            "ownerEmail": owner_email,
            "createdAt": row.created_at,
            "origin": if team { "team" } else { "direct" },
            "orgName": org_name,
        }));
    }
    Json(out).into_response()
}

/// `POST /api/shares/{id}/accept` — the grantee accepts (materialises the grant).
pub async fn accept(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    respond(&hub, &headers, &id, true).await
}

/// `POST /api/shares/{id}/decline` — the grantee declines.
pub async fn decline(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    respond(&hub, &headers, &id, false).await
}

async fn respond(hub: &HubState, headers: &HeaderMap, id: &str, accept: bool) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    outcome_to_response(hub.share_respond(&actor, id, accept).await)
}

/// `POST /api/shares/{id}/revoke` — the inviter cancels (pre- or post-accept).
/// Falls through to the email-invite table (proposal 0056 Part C) so the
/// outbox's Cancel works on an `"invited"` row too.
pub async fn revoke(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    // Team rows are managed by membership, never individually (0065 A4): the way
    // out is the owner's per-machine opt-out or a membership change — an
    // individual revoke would silently diverge and be resurrected by the nightly
    // reconcile.
    #[cfg(feature = "multi-tenant")]
    if let Some(store) = hub.store() {
        if store.share_is_team(&id).await {
            return (
                StatusCode::CONFLICT,
                "managed by your team — hide the machine from Settings instead",
            )
                .into_response();
        }
    }
    match hub.share_revoke(&actor, &id).await {
        ShareOutcome::NotFound => {
            if hub.email_invite_revoke(&actor, &id).await {
                (StatusCode::OK, Json(json!({ "status": "revoked" }))).into_response()
            } else {
                (StatusCode::NOT_FOUND, "no such invite").into_response()
            }
        }
        outcome => {
            // Team-history audit (0063 D1): a member's share activity is
            // org-visible; a lone user's sharing stays unlogged, as today.
            if matches!(outcome, ShareOutcome::Ok(ref s) if s == "revoked") {
                if let Some(store) = hub.store() {
                    if let Some((org, _)) = store.org_for_user(&actor).await {
                        store.audit_append(&org.id, Some(&actor), "share.revoked", Some(&id), None).await;
                    }
                }
            }
            outcome_to_response(outcome)
        }
    }
}

/// `POST /api/shares/received/{id}/leave` — the grantee gives back a share they
/// hold (the "Leave" action). `id` is the received grant's id.
pub async fn leave(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    // Team rows refuse individual leave (0065 A4): membership implies them and
    // the nightly reconcile would just resurrect the row.
    #[cfg(feature = "multi-tenant")]
    if let Some(store) = hub.store() {
        if store.share_is_team(&id).await {
            return (
                StatusCode::CONFLICT,
                "managed by your team — ask the owner to hide this machine, or leave the team",
            )
                .into_response();
        }
    }
    if hub.leave_grant(&actor, &id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "no such share").into_response()
    }
}

fn outcome_to_response(outcome: ShareOutcome) -> Response {
    match outcome {
        ShareOutcome::Ok(status) => (StatusCode::OK, Json(json!({ "status": status }))).into_response(),
        ShareOutcome::Conflict => (StatusCode::CONFLICT, "this invite is no longer pending").into_response(),
        ShareOutcome::NotFound => (StatusCode::NOT_FOUND, "no such invite").into_response(),
    }
}

/// Is a session invite's underlying session gone? Lazy validation (§6): the hub
/// gets no durable "session deleted" event, so we check the owning agent's current
/// session list. If the agent isn't connected this run we can't prove it dead, so
/// we keep showing the row.
fn session_dead(hub: &HubState, row: &ShareInviteRow) -> bool {
    if row.resource_kind != "session" {
        return false;
    }
    let Some(name) = row.session_name.as_deref() else { return false };
    match hub.registry.get(&row.agent_id) {
        Some(agent) => !agent.sessions_tagged().iter().any(|s| s.name == name),
        None => false,
    }
}

/// The inbox/outbox DTO (the contract [0041] renders). Emails are resolved so
/// neither side sees a raw `user_id`; `permission` is derived from the kind
/// ("use" for an agent, "view" for a session).
async fn invite_view(hub: &HubState, row: &ShareInviteRow) -> Value {
    let inviter_email = hub.user_email(&row.inviter_user_id).await;
    let grantee_email = hub.user_email(&row.grantee_user_id).await;
    let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone());
    let permission = if row.resource_kind == "session" { "view" } else { "use" };
    json!({
        "id": row.id,
        "inviterEmail": inviter_email,
        "granteeEmail": grantee_email,
        "resourceKind": row.resource_kind,
        "agentId": row.agent_id,
        "machine": machine,
        "session": row.session_name,
        "permission": permission,
        "ownerPeek": row.owner_peek,
        "status": row.status,
        "createdAt": row.created_at,
        "expiresAt": row.expires_at,
    })
}
