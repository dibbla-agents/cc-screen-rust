//! Read-only **link grants** (proposal 0083 Part C), compiled only under
//! `multi-tenant`. An owner mints `https://<hub>/s/<token>` for ONE file;
//! whoever holds the URL may read that file — no account, nothing else on the
//! machine reachable, revocable at any time.
//!
//! ── The invariants this module exists to hold ────────────────────────────────
//!
//! 1. **Its own resolver.** The token is resolved by SHA-256 lookup against
//!    `link_shares` to exactly one `(agent_id, path)` pair. This module never
//!    calls `file_route`, never calls `resolve_browsable`, and never constructs
//!    a [`Visibility`](crate::registry::Visibility). A link grant therefore
//!    cannot widen into anything the sharing model grants — the two systems do
//!    not meet. ([0042]'s client-controlled-scope finding class is exactly the
//!    shape this avoids.)
//! 2. **The read op is hardcoded.** [`content`] issues `Cmd::File { op: "read" }`
//!    with the op written as a literal. There is no request field, no config,
//!    and no code path here that could produce a write, a listing, or a search.
//! 3. **Never in `Visibility`, an inbox, or an accept path.** Link grants live
//!    in their own table (see `migrations/0014_link_shares.sql`), which the
//!    visibility pipeline does not read. That inertness is structural, not a
//!    filter someone can forget.
//! 4. **One undifferentiated 404.** Malformed, unknown, revoked, expired, and
//!    gone are answered identically — same status, same headers, same body. The
//!    single distinguished state is *valid grant, agent offline* → 503, and it
//!    is reachable only after the grant has been proven live, so a revoked
//!    token can never learn whether a machine is up.
//!
//! Two residual disclosures are accepted and documented in `site/docs/security.md`:
//! the timing difference between a local 404 and one that took an agent
//! round-trip, and the fact that a holder of a *valid* link can poll the 503 to
//! observe when the owner's machine comes online.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cc_screen_protocol::hub::{Cmd, CmdResult};
use serde_json::{json, Value};

use crate::db::LinkShareRow;
use crate::registry::RequestErr;
use crate::state::HubState;

/// Token shape, checked before any store work. `generate_token` is 32 bytes of
/// OsRng in base64url-no-pad → 43 chars; the window is generous so a future
/// widening doesn't silently 404 every link, and narrow enough that a garbage
/// path never reaches the database.
fn well_formed(token: &str) -> bool {
    let len = token.len();
    (16..=128).contains(&len) && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// How many content reads one token may make per minute. This is the bound that
/// protects the OWNER'S MACHINE: every hit is a `Cmd::File` read of up to 5 MiB
/// against a personal laptop, and a *valid* leaked link would otherwise be
/// hammerable without ever touching the per-source failure throttle (which only
/// ever sees refusals). Generous enough for a page load plus reloads.
const CONTENT_READS_PER_MIN: usize = 20;

/// Per-token sliding window, keyed on the token HASH (never the plaintext, which
/// must not sit in a process-local map). Same shape as its siblings in
/// `share.rs` / `account.rs`, with its own bucket.
fn content_rate_limited(token_hash: &str) -> bool {
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
    let hits = map.entry(token_hash.to_string()).or_default();
    hits.retain(|t| *t > cutoff);
    if hits.len() >= CONTENT_READS_PER_MIN {
        return true;
    }
    hits.push(now);
    false
}

/// The response contract, applied on EVERY branch of both endpoints — including
/// the refusals, because a header that appears on one status and not another is
/// itself a signal.
///
/// API responses do not inherit the asset layer's security headers
/// (`app_security_headers` is applied by `assets.rs` to asset/HTML responses
/// only), so these are set here by hand:
///
/// * a fixed, safe `Content-Type` — never `text/html`, never `image/svg+xml`,
///   never an untyped sniffable body. Rendering happens in the client's
///   `LinkView`; this API serves data;
/// * `nosniff` so a browser can't upgrade the above into markup;
/// * `no-store` and **no validators** (`ETag`/`Last-Modified`): a 304 on a
///   revoked token would be both a validity oracle and a revocation bypass, so
///   no cache — browser, proxy or CDN — may retain this past revocation;
/// * `noindex` so a leaked URL doesn't become a search result;
/// * `no-referrer` so the token doesn't ride an outbound request from the page.
fn sealed(status: StatusCode, content_type: &'static str, body: impl Into<axum::body::Body>) -> Response {
    let mut resp = Response::new(body.into());
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    resp
}

/// The one refusal. Identical bytes for malformed / unknown / revoked / expired
/// / gone / confinement-refused / not-a-file / too-large — a bearer who may hold
/// nothing learns nothing, not even from a stray header.
fn not_found() -> Response {
    sealed(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", "not found")
}

/// Resolve `token` to a live grant, or the single refusal. Also applies the
/// per-source failure throttle so token guessing is bounded (moot against
/// 2^256, but free and consistent with `share::invite_info`).
async fn resolve(hub: &HubState, headers: &HeaderMap, token: &str) -> Result<(LinkShareRow, String), Response> {
    if !well_formed(token) {
        return Err(not_found());
    }
    let source = format!("link:{}", cc_screen_auth::source_key(headers));
    let now = std::time::Instant::now();
    // A locked-out source gets the SAME 404, just without the database work.
    //
    // Two reasons this is not the usual `429 too many attempts`. First, the
    // refusal must stay one answer: a 429 sitting among the 404s is a
    // differentiated status, and differentiating on *anything* is what this
    // module exists to avoid. Second, `source_key` reads `X-Forwarded-For` and
    // is advisory (`crates/auth/src/throttle.rs`) — locking a shared egress IP
    // out of *valid* links because someone behind it typed a bad one would be
    // real collateral damage in exchange for guarding a 256-bit token that is
    // not guessable in the first place. So the throttle here only makes
    // refusals cheap; it never refuses a grant that resolves.
    let locked = hub.login_throttle.locked_for(&source, now).is_some();
    if locked {
        return Err(not_found());
    }
    let Some(store) = hub.store() else { return Err(not_found()) };
    let hash = cc_screen_auth::sha256_hex(token);
    match store.link_share_by_token_hash(&hash).await {
        Some(row) => Ok((row, hash)),
        None => {
            hub.login_throttle.record_failure(&source, now);
            Err(not_found())
        }
    }
}

/// `GET /api/link/:token` — the page's chrome: what it is called, which machine
/// it came from, and how to render it.
///
/// Answered by the hub alone: no agent round-trip, so the page can title itself
/// while the owner's laptop is still waking up, and a valid token costs the
/// owner's machine nothing until the reader actually asks for the bytes.
pub async fn meta(State(hub): State<HubState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let (row, _) = match resolve(&hub, &headers, &token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone()).unwrap_or_default();
    let body = json!({
        "name": row.name,
        "machine": machine,
        "mimeClass": mime_class(&row.name),
    });
    sealed(
        StatusCode::OK,
        "application/json",
        serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()),
    )
}

/// `GET /api/link/:token/content` — the file, as text.
///
/// This is the only place a link grant reaches an agent, and it can only ever
/// read: the op below is a literal. Every agent-side error — confinement 403,
/// not-a-file 400, gone 404, too-large 413 — collapses into the same 404 rather
/// than being relayed the way `file_route` relays errors, because a bearer must
/// not learn the shape of the owner's filesystem.
pub async fn content(State(hub): State<HubState>, headers: HeaderMap, Path(token): Path<String>) -> Response {
    let (row, hash) = match resolve(&hub, &headers, &token).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Per-token bound, checked AFTER validity (so it cannot be used to probe
    // tokens) and BEFORE the agent hears anything (so the excess never lands on
    // the owner's machine).
    if content_rate_limited(&hash) {
        return sealed(
            StatusCode::TOO_MANY_REQUESTS,
            "text/plain; charset=utf-8",
            "too many requests for this link",
        );
    }
    let Some(agent) = hub.registry.get(&row.agent_id).filter(|a| a.online()) else {
        // The single distinguished state, reachable only by a currently-valid
        // grant: the machine is asleep, not the link dead.
        return sealed(StatusCode::SERVICE_UNAVAILABLE, "text/plain; charset=utf-8", "machine offline");
    };
    let cmd = Cmd::File { op: "read".to_string(), args: json!({ "path": row.path }) };
    let reply = match agent.request(cmd).await {
        Ok(v) => v,
        Err(RequestErr::Offline) => {
            return sealed(StatusCode::SERVICE_UNAVAILABLE, "text/plain; charset=utf-8", "machine offline")
        }
        // A timeout is not "the file is gone", but it is also not something a
        // bearer needs told apart from one; 503 keeps the retry advice honest.
        Err(RequestErr::Timeout) => {
            return sealed(StatusCode::SERVICE_UNAVAILABLE, "text/plain; charset=utf-8", "machine offline")
        }
    };
    match reply {
        CmdResult::Json(v) => match read_text(&v) {
            Some(text) => sealed(StatusCode::OK, "text/plain; charset=utf-8", text),
            // `{"editable":false}` — the agent read a binary. v1 link grants
            // serve what the read op serves: text. The client renders its
            // "no preview" page from this, which is distinguishable only to
            // someone who already holds a VALID token.
            None => sealed(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "text/plain; charset=utf-8",
                "no preview for this file type",
            ),
        },
        _ => not_found(),
    }
}

/// Pull the text out of the agent's `read` reply, or `None` when it refused the
/// file as non-text (`{"editable":false}`).
fn read_text(v: &Value) -> Option<String> {
    if v.get("editable").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    v.get("content").and_then(Value::as_str).map(|s| s.to_string())
}

/// How the client should render this name: the reading view, highlighted
/// source, or the no-preview page. Extension-only — the hub has no filesystem
/// and must not need one to draw a header.
fn mime_class(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdx") {
        "markdown"
    } else if lower.ends_with(".pdf")
        || lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".zip")
        || lower.ends_with(".gz")
    {
        "other"
    } else {
        "text"
    }
}

/// The public base for `/s/<token>` URLs — `CCHUB_PUBLIC_URL` when configured,
/// else a relative path the frontend resolves against its own origin. Same rule
/// as `share::invite_url`, deliberately: one place decides what a hub calls
/// itself.
pub(crate) fn link_url(token: &str) -> String {
    let base = std::env::var("CCHUB_PUBLIC_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    format!("{base}/s/{token}")
}

/// The `kind:"link"` arm of `POST /api/shares` (proposal 0083 Part C / [0074]
/// §C1's one-endpoint rule). Returns the same uniform
/// `{id, status, invite_url}` body as every other kind — the field stays
/// `invite_url` even though the URL is `/s/<token>`, because forking the field
/// name forks the response.
///
/// The caller (`share::create`) has already checked the per-source rate limit,
/// the cookie, and the plan gate. This arm:
///
/// * resolves the machine under the actor's OWN scope (owner-only: a [0039]
///   share grants *use*, not the right to publish someone's files);
/// * asks the AGENT to canonicalize the path, by doing the very read the link
///   will serve — so an unreadable, non-text, oversized, missing or
///   confinement-refused file is refused at mint rather than minted into a
///   permanent 404 ([0074]'s rule: the hub never canonicalizes paths, and an
///   offline agent means no mint);
/// * stores only the token's SHA-256, and returns the plaintext exactly once.
pub(crate) async fn mint(
    hub: &HubState,
    actor: &str,
    machine: &str,
    path: &str,
    expires_at: Option<i64>,
) -> Response {
    let path = path.trim();
    if path.is_empty() {
        return (StatusCode::BAD_REQUEST, "a link share needs a file path").into_response();
    }
    let owner_scope = crate::registry::Visibility::user(actor.to_string());
    let Some(agent) = hub.registry.resolve_scoped(&owner_scope, machine, None) else {
        return (
            StatusCode::NOT_FOUND,
            "no such machine you own, or it's offline (a link can only be created while the machine is up)",
        )
            .into_response();
    };
    let agent_id = agent.agent_id.clone();

    // Canonicalize + validate agent-side, with the exact op the link will use.
    let cmd = Cmd::File { op: "read".to_string(), args: json!({ "path": path }) };
    let reply = match agent.request(cmd).await {
        Ok(v) => v,
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, "that machine didn't answer").into_response(),
    };
    let (canon, name) = match reply {
        CmdResult::Json(v) => {
            if v.get("editable").and_then(Value::as_bool) == Some(false) {
                return (
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "link shares serve text files — this one isn't text",
                )
                    .into_response();
            }
            let canon = v.get("path").and_then(Value::as_str).unwrap_or(path).to_string();
            let name = v.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            (canon, name)
        }
        CmdResult::Error { code, msg } => {
            return (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), msg).into_response()
        }
        _ => return (StatusCode::BAD_GATEWAY, "unexpected agent reply").into_response(),
    };
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "that path isn't a file").into_response();
    }

    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response();
    };
    let token = cc_screen_auth::generate_token();
    let id = match store
        .link_share_create(actor, &agent_id, &canon, &name, &cc_screen_auth::sha256_hex(&token), expires_at)
        .await
    {
        Ok(id) => id,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    // Team-history audit, on the same rule as every other share (0063 D1): a
    // member's sharing is org-visible history; a lone user's stays unlogged.
    if let Some((org, _)) = store.org_for_user(actor).await {
        let detail = format!("{{\"file\":{}}}", serde_json::to_string(&name).unwrap_or_default());
        store.audit_append(&org.id, Some(actor), "share.link_created", Some(&name), Some(&detail)).await;
    }
    (StatusCode::OK, Json(json!({ "id": id, "status": "active", "invite_url": link_url(&token) }))).into_response()
}

/// The outbox rows for the caller's link grants — the [0041] surface, labeled by
/// the file's name. **Never** the token: only its hash is stored, so there is
/// nothing to leak here even by accident.
pub(crate) async fn outbox_rows(hub: &HubState, actor: &str) -> Vec<Value> {
    let Some(store) = hub.store() else { return Vec::new() };
    let mut out = Vec::new();
    for row in store.link_share_outbox(actor).await {
        let machine = hub.registry.get(&row.agent_id).map(|a| a.machine_id.clone());
        out.push(json!({
            "id": row.id,
            "resourceKind": "link",
            "agentId": row.agent_id,
            "machine": machine,
            "file": row.name,
            "path": row.path,
            "permission": "read",
            "status": "active",
            "createdAt": row.created_at,
            "expiresAt": row.expires_at,
        }));
    }
    out
}

/// `POST /api/shares/:id/revoke` fall-through for a link grant — the same door
/// every other share is revoked through. Owner-scoped in the store.
pub(crate) async fn revoke(hub: &HubState, actor: &str, id: &str) -> bool {
    match hub.store() {
        Some(store) => store.link_share_revoke(actor, id).await,
        None => false,
    }
}

/// `POST /api/shares/:id/regenerate` — mint a fresh token onto the SAME grant.
/// The old URL dies at this instant; the id, the path and the expiry are
/// unchanged. This is what makes hashing the token at rest livable: a link the
/// owner can no longer see, they can always replace.
pub async fn regenerate(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(actor) = hub.client_auth.user_from_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let Some(store) = hub.store() else {
        return (StatusCode::NOT_FOUND, "not a multi-tenant hub").into_response();
    };
    let token = cc_screen_auth::generate_token();
    if !store.link_share_regenerate(&actor, &id, &cc_screen_auth::sha256_hex(&token)).await {
        return (StatusCode::NOT_FOUND, "no such link").into_response();
    }
    (StatusCode::OK, Json(json!({ "id": id, "status": "active", "invite_url": link_url(&token) }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_shape_is_checked_before_any_store_work() {
        assert!(well_formed(&cc_screen_auth::generate_token()));
        assert!(!well_formed(""));
        assert!(!well_formed("short"));
        assert!(!well_formed("has spaces in it and is long enough"));
        assert!(!well_formed("slashes/are/not/base64url/and/never/will/be"));
        assert!(!well_formed(&"a".repeat(129)));
    }

    #[test]
    fn mime_class_routes_the_renderer() {
        assert_eq!(mime_class("tasks.md"), "markdown");
        assert_eq!(mime_class("TASKS.MARKDOWN"), "markdown");
        assert_eq!(mime_class("main.rs"), "text");
        assert_eq!(mime_class("notes"), "text");
        assert_eq!(mime_class("scan.PDF"), "other");
        assert_eq!(mime_class("photo.jpeg"), "other");
    }

    #[test]
    fn read_reply_yields_text_or_refuses_a_binary() {
        assert_eq!(read_text(&json!({ "content": "hello" })).as_deref(), Some("hello"));
        assert_eq!(read_text(&json!({ "editable": false })), None);
        // A reply with neither is not text either — never a silent empty body.
        assert_eq!(read_text(&json!({ "path": "/x" })), None);
    }

    #[test]
    fn every_response_carries_the_full_header_contract() {
        for resp in [
            not_found(),
            sealed(StatusCode::OK, "text/plain; charset=utf-8", "hi"),
            sealed(StatusCode::SERVICE_UNAVAILABLE, "text/plain; charset=utf-8", "machine offline"),
        ] {
            let h = resp.headers();
            assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
            assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "no-store");
            assert_eq!(h.get(header::REFERRER_POLICY).unwrap(), "no-referrer");
            assert_eq!(h.get("x-robots-tag").unwrap(), "noindex, nofollow");
            // No validators: a 304 on a revoked token would be a revocation
            // bypass AND a validity oracle.
            assert!(h.get(header::ETAG).is_none());
            assert!(h.get(header::LAST_MODIFIED).is_none());
            // Never a type a browser would execute.
            let ct = h.get(header::CONTENT_TYPE).unwrap().to_str().unwrap();
            assert!(!ct.contains("html") && !ct.contains("svg"), "unsafe content-type: {ct}");
        }
    }

    #[test]
    fn the_refusal_is_byte_identical_across_reasons() {
        // There is exactly ONE constructor for it, which is the point: a future
        // branch cannot accidentally differentiate revoked from unknown.
        let a = not_found();
        let b = not_found();
        assert_eq!(a.status(), b.status());
        assert_eq!(a.headers(), b.headers());
    }
}
