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

/// `POST /api/shares` — create (or re-offer) an invite to a user by email.
pub async fn create(State(hub): State<HubState>, headers: HeaderMap, Json(req): Json<CreateShareReq>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let Some(grantee) = hub.user_id_by_email(&req.grantee_email).await else {
        return (StatusCode::NOT_FOUND, "no account with that email").into_response();
    };
    if grantee == actor {
        return (StatusCode::BAD_REQUEST, "cannot share with yourself").into_response();
    }
    let session = req.session.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let kind = if session.is_some() { "session" } else { "agent" };

    // Resolve the machine under the inviter's OWN scope (no grants) — you can only
    // invite to a resource you own (proposal 0040 §5, the §4.1 isolation keystone).
    let owner_scope = Visibility::user(actor.clone());
    let Some(agent) = hub.registry.resolve_scoped(&owner_scope, &req.machine, session) else {
        return (StatusCode::NOT_FOUND, "no such machine/session you own (try ?machine=)").into_response();
    };
    let agent_id = agent.agent_id.clone();
    let machine_label = agent.machine_id.clone();

    match hub.share_create(&actor, &grantee, kind, &agent_id, session, req.owner_peek).await {
        Ok((id, status)) => {
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
            (StatusCode::OK, Json(json!({ "id": id, "status": status }))).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
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

/// `GET /api/shares/outbox` — the invites the caller has sent (all statuses).
pub async fn outbox(State(hub): State<HubState>, headers: HeaderMap) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let mut out = Vec::new();
    for row in hub.share_outbox(&actor).await {
        out.push(invite_view(&hub, &row).await);
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
        // currently-connected agent (best-effort, §6 of 0040).
        if row.kind == "session" {
            if let (Some(agent), Some(name)) = (hub.registry.get(&row.agent_id), row.session.as_deref()) {
                if !agent.sessions_tagged().iter().any(|s| s.name == name) {
                    continue;
                }
            }
        }
        let owner_email = hub.user_email(&row.owner_user_id).await;
        let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone());
        out.push(json!({
            "id": row.id,
            "agentId": row.agent_id,
            "machine": machine,
            "session": row.session,
            "kind": row.kind,
            "permission": if row.kind == "session" { "view" } else { "use" },
            "ownerEmail": owner_email,
            "createdAt": row.created_at,
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
pub async fn revoke(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    outcome_to_response(hub.share_revoke(&actor, &id).await)
}

/// `POST /api/shares/received/{id}/leave` — the grantee gives back a share they
/// hold (the "Leave" action). `id` is the received grant's id.
pub async fn leave(State(hub): State<HubState>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
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
