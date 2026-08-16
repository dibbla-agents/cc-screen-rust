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
    /// The recipient, named by account email. Defaulted since 0083: a
    /// `kind:"link"` share has no recipient at all and must not have to send an
    /// empty string to satisfy the extractor. Every email-bearing arm still
    /// runs `validate_email_address`, so an omitted address 400s there exactly
    /// as an empty one always has.
    #[serde(default)]
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
    /// Proposal 0083 Part C — `"link"` selects the read-only link grant: one
    /// file, no grantee, the URL is the recipient. Absent/empty keeps the
    /// pre-0083 behaviour byte-for-byte, so an older client is unaffected.
    #[serde(default)]
    kind: Option<String>,
    /// The file a link grant serves (absolute on the agent; canonicalized
    /// agent-side at mint). Ignored by every other kind.
    #[serde(default)]
    path: Option<String>,
    /// Optional expiry, unix seconds. NULL/absent = until revoked.
    #[serde(default)]
    expires_at: Option<i64>,
}

// ── The invite abuse bounds (proposal 0073 C1) ────────────────────────────────
// Shared by BOTH invite endpoints (`share::create` and `org::invite_create`)
// rather than duplicated per module: one bucket, one ceiling helper. If each
// endpoint kept its own, a caller could double its budget just by alternating
// between them.

/// How many invitations one source may POST per rolling minute, across both
/// invite endpoints. `POST /api/orgs/invites` had **no** rate limit of any kind
/// before this (proposal 0073 C1); a row-spam nuisance while nothing was sent,
/// a deliverability and sender-reputation exposure the moment mail leaves an
/// authenticated `ccscreen.dev`.
const INVITES_PER_MIN: usize = 30;

/// Per-source sliding window for the two invite endpoints — the same shape as
/// signup's (`account::signup_rate_limited`) and the device flow's
/// (`device::code_rate_limited`), with its own bucket.
///
/// Be honest about what it is worth: `source_key` reads `X-Forwarded-For`, an IP
/// the caller chooses, and the bucket is a process-local `HashMap` that resets on
/// restart. It bounds a clumsy loop, not a determined attacker — the live-invite
/// caps and the send ceilings are what do the real work.
pub(crate) fn invite_rate_limited(source: &str) -> bool {
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
    if hits.len() >= INVITES_PER_MIN {
        return true;
    }
    hits.push(now);
    false
}

/// How many invitations one **destination address** may be mailed per rolling
/// 24h, across every inviter and every org on this hub.
///
/// This is the one bound that speaks for the person who never asked to be here.
/// Every other bound limits how much one account or one org can *send*; none of
/// them stops an attacker rotating free, unverified accounts (signup mints the
/// cookie inline and verifies nothing) and pointing all of them at one victim.
/// Generous enough for a real invitation plus a couple of Resends, and for two
/// different teams inviting the same person on the same day.
const DEST_MAILS_PER_DAY: usize = 5;

/// The destination-address frequency cap. Process-local like its siblings (and
/// like them, reset by a restart) — the durable fix is the email-verification
/// proposal named in 0073's Non-Goals.
fn destination_over_cap(to: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static SENDS: OnceLock<Mutex<HashMap<String, Vec<Instant>>>> = OnceLock::new();
    let mut map = SENDS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    let now = Instant::now();
    let cutoff = now - Duration::from_secs(86_400);
    if map.len() >= 4096 {
        map.retain(|_, v| v.iter().any(|t| *t > cutoff));
    }
    let hits = map.entry(to.to_string()).or_default();
    hits.retain(|t| *t > cutoff);
    if hits.len() >= DEST_MAILS_PER_DAY {
        return true;
    }
    hits.push(now);
    false
}

/// May this invitation be mailed? The two send ceilings (per-org daily,
/// hub-wide daily) plus the destination-address cap, evaluated **in the handler,
/// before the spawn** — so an invitation over a ceiling spawns no task at all,
/// which is the acceptance criterion holding by construction.
///
/// Deliberately not inside `Mailer::send`: it takes no account identity and holds
/// no counters, so "enforce it in the mailer so every future caller inherits it"
/// has nowhere to keep state.
///
/// A `false` never fails the invitation. The row is created either way and the
/// copyable link — the channel that has always worked — is still returned; only
/// the send is skipped, and `delivery` stays NULL, which is exactly what the
/// 0013 migration defines it to mean ("no send was attempted"). Refusing the
/// *invitation* because the hub is near a relay quota would turn a mail problem
/// into a product outage.
///
/// `org_id` is the inviting org when there is one (a lone Pro user's sharing has
/// none, by design). Both callers reach here with the address already normalized,
/// and the check is identical whether or not that address has an account.
pub(crate) async fn may_send_invite_mail(
    store: &std::sync::Arc<dyn crate::db::Store>,
    org_id: Option<&str>,
    to: &str,
) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let since = now - 86_400;
    let hub_today = store.invites_emailed_since(since).await;
    // Warn well before the quota, not at it: at the ceiling the operator has
    // already lost sends, and the relay's own quota is a cliff with no signal.
    if hub_today * 5 >= crate::db::HUB_MAIL_PER_DAY * 4 {
        tracing::warn!(
            "invite mail: {hub_today}/{} sent in the last 24h — approaching the hub ceiling",
            crate::db::HUB_MAIL_PER_DAY
        );
    }
    if hub_today >= crate::db::HUB_MAIL_PER_DAY {
        tracing::warn!("invite mail: hub daily ceiling reached — invitation created, not mailed");
        return false;
    }
    if let Some(org) = org_id {
        if store.org_invites_emailed_since(org, since).await >= crate::db::ORG_MAIL_PER_DAY {
            tracing::warn!("invite mail: org {org} daily ceiling reached — invitation created, not mailed");
            return false;
        }
    }
    if destination_over_cap(to) {
        tracing::warn!("invite mail: destination frequency cap reached — invitation created, not mailed");
        return false;
    }
    true
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
    // Per-source window first (proposal 0073 C1) — the cheapest bound, so a loop
    // is refused before any store work. It does NOT bound unauthenticated
    // callers, despite sitting above the cookie check: the client-auth layer
    // refuses them before this handler runs (measured — 40/40 anonymous POSTs
    // answer 401 without reaching here). What it does bound is an *authenticated*
    // loop, including one whose requests then 400 or 404.
    if invite_rate_limited(&cc_screen_auth::source_key(&headers)) {
        return (StatusCode::TOO_MANY_REQUESTS, "too many invitations — slow down").into_response();
    }
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
    // Proposal 0083 Part C — the link arm, taken BEFORE the grantee-email
    // validation below. That validation used to sit above every arm ([0073] C3
    // put it there so a bad address 400s before any row is written and before
    // the account-existence arms diverge); a link grant has no grantee at all,
    // so it would have failed on an empty string it is never supposed to carry.
    // Moving the arm above it is the one real fork this proposal needs, and it
    // is safe precisely because the two paths share no downstream state: the
    // email validation still guards BOTH email-bearing arms, unchanged.
    if req.kind.as_deref() == Some("link") {
        return crate::link::mint(
            &hub,
            &actor,
            &req.machine,
            req.path.as_deref().unwrap_or_default(),
            req.expires_at,
        )
        .await;
    }
    // Address validation at the API boundary (proposal 0073 C3): 400 before any
    // row is written, and before the arms diverge — the answer is deterministic
    // from the request alone, so it discloses nothing about accounts.
    let grantee_email = crate::db::normalize_email(&req.grantee_email);
    if let Err(e) = crate::db::validate_email_address(&grantee_email) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let session = req.session.as_deref().map(str::trim).filter(|s| !s.is_empty());
    // The session name is interpolated into the invitation's SUBJECT via `what`
    // below, and it is *not* checked for existence: `resolve_scoped` short
    // -circuits `may_see_session` for an agent the caller owns, so an owner can
    // POST an arbitrary string here. `lettre` RFC 2047-encodes it, so this is
    // defense in depth rather than a live hole — but C3's own argument is that
    // resting a class of injection on one library's parser is an assumption that
    // survives right up until someone switches transports, and a value with no
    // length cap folded into one header is worth bounding regardless.
    if let Some(s) = session {
        if s.len() > 128 || s.chars().any(crate::db::is_control_char) {
            return (StatusCode::BAD_REQUEST, "invalid session name").into_response();
        }
    }
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

    // The live-invite cap, PRE-FLIGHT (proposal 0073 C1) — before the arms, so it
    // fires identically whether or not the address has an account. The known-user
    // arm calls `email_invite_create` only *after* `share_create` has written a
    // row, so a cap discovered down there could only be swallowed; the bound
    // would then be reachable in the unknown arm alone, which is precisely the
    // account-existence oracle [0042] forbids. The store still enforces it.
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response();
    };
    if store.email_invite_cap_exceeded(&actor, &grantee_email, kind, &agent_id, session).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many pending invitations — cancel some from your outbox first",
        )
            .into_response();
    }

    // Hoisted out of the known-account arm (proposal 0073, "the delivery
    // decision" §2): the mail body needs both in BOTH arms, and `what` must not
    // differ between them. Doing this work only when the address has an account
    // was itself a timing signal.
    let inviter_email = hub.user_email(&actor).await.unwrap_or_default();
    // `machine_label` is whatever the agent registered itself as — the user's own
    // choice, so it gets the same treatment as the session name above. It is
    // scrubbed rather than refused: a share must not start failing because a
    // machine somewhere has an odd name, and this string is only ever *rendered*.
    let machine_label: String = machine_label.chars().filter(|c| !crate::db::is_control_char(*c)).take(128).collect();
    let what = match session {
        Some(s) => format!("session {s} on {machine_label}"),
        None => format!("machine {machine_label}"),
    };

    // `mail_id` is the EMAIL-INVITE row id — the row the delivery receipt lives
    // on (0073 B2). It is deliberately not the response's `id`, which stays the
    // share-invite id in the known arm exactly as before.
    //
    // Known residual, recorded so it isn't rediscovered as new: the two arms are
    // NOT timing-uniform here and 0073 does not make them so. The known arm must
    // write a `share_invites` row (`hub.share_create`) that the unknown arm must
    // not, and that extra durable commit measures ~5 ms — enough to classify a
    // single probe at ~92%. It is structural and pre-existing (measured at the
    // same magnitude on `main`, where it classifies at ~99% because there is less
    // symmetric work either side of it), and 0073 strictly *reduces* the total
    // asymmetry by spawning the push that used to run in this arm alone. Closing
    // it needs the unknown arm to do equivalent work, which is its own change.
    let (id, status, mail_id, token) = match hub.user_id_by_email(&grantee_email).await {
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
            // buzz never loses the invite — it sits in /inbox. Spawned like
            // `uplink_server.rs:187`: `notify_scoped` is a blocking FCM/APNs call,
            // and awaiting it here would add hundreds of milliseconds in the
            // account-exists arm alone (0073's timing constraint).
            let (push, body) = (hub.push.clone(), format!("{inviter_email} shared {what} with you"));
            tokio::spawn(async move {
                push.notify_scoped(Some(&grantee), "New share invitation", &body, "share-invite").await;
            });
            // Mint the link token (pre-converted — it identifies, never grants).
            let (mail_id, token) = hub
                .email_invite_create(&actor, &grantee_email, kind, &agent_id, session, req.owner_peek, true)
                .await
                .unwrap_or_default();
            (id, status, mail_id, token)
        }
        None => {
            match hub
                .email_invite_create(&actor, &grantee_email, kind, &agent_id, session, req.owner_peek, false)
                .await
            {
                Ok((id, token)) => (id.clone(), "pending".to_string(), id, token),
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
    let mut audit_org: Option<String> = None;
    if let Some(store) = hub.store() {
        if let Some((org, _)) = store.org_for_user(&actor).await {
            let detail = format!(
                "{{\"machine\":{},\"kind\":\"{kind}\"}}",
                serde_json::to_string(&machine_label).unwrap_or_default()
            );
            store
                .audit_append(&org.id, Some(&actor), "share.created", Some(&grantee_email), Some(&detail))
                .await;
            audit_org = Some(org.id);
        }
    }
    // Mail the invite (proposal 0073 B1). Spawned, never awaited, for the same
    // reasons as the org path — and asymmetric to it in three ways: the receipt
    // lives on the email-invite row (`mail_id`), the audit action is
    // `share.emailed`, and there may be NO org to audit against (a lone Pro
    // user's sharing is deliberately unlogged — the log line is that trail).
    // `status` here can be `"accepted"` from `share_create`, i.e. this exact
    // share already exists and was accepted: no mail.
    //
    // The send ceilings (0073 C1) are checked HERE, before the spawn, so an
    // invitation over one spawns no task at all. They never fail the invitation
    // — the row and the copyable link stand; only the send is skipped.
    if status == "pending"
        && !token.is_empty()
        && hub.mailer.active()
        && may_send_invite_mail(&store, audit_org.as_deref(), &grantee_email).await
    {
        let mailer = hub.mailer.clone();
        let (id, token) = (mail_id.clone(), token.clone());
        let (to, inviter, what) = (grantee_email.clone(), inviter_email.clone(), what.clone());
        tokio::spawn(async move {
            let (subject, body) = crate::mailer::share_invite_message(&inviter, &what, &invite_url(&token));
            if !store.email_invite_mark_sending(&id, &token).await {
                return; // revoked or superseded — this attempt writes nothing
            }
            let outcome = mailer.send(&to, &subject, &body).await;
            store.email_invite_delivery_record(&id, &token, outcome.as_str()).await;
            tracing::info!("share invite mail to {to}: {}", outcome.detail());
            if let Some(org_id) = audit_org {
                store.audit_append(&org_id, None, "share.emailed", Some(&to), Some(&outcome.detail())).await;
            }
        });
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
    // Read-only link grants (proposal 0083 Part C), labeled by the file's name.
    // Never the token — only its hash is stored, so there is nothing here to
    // leak even by accident; managing one means Revoke or Regenerate.
    out.extend(crate::link::outbox_rows(&hub, &actor).await);
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
            // The fall-through chain: a share invite, then an email invite, then
            // a link grant (proposal 0083 Part C). One Revoke action, every kind
            // of share — the client doesn't have to know which table it's in.
            if hub.email_invite_revoke(&actor, &id).await || crate::link::revoke(&hub, &actor, &id).await {
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
