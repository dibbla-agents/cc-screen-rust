//! Multi-tenant tenant-isolation tests (proposal 0001 Phase 1 "Done when").
//!
//! Two hand-created users, each owning an agent that happens to share the machine
//! label "laptop", talk to the **real** multi-tenant hub router. We assert the
//! §4.1 keystone end-to-end: neither user can list, control, or attach to the
//! other's agent, and an unauthenticated request is refused. Only compiled with
//! `--features multi-tenant`.
#![cfg(feature = "multi-tenant")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cc_screen_auth::Auth;
use cc_screen_hub::db::{SqliteStore, Store};
use cc_screen_hub::registry::Registry;
use cc_screen_hub::state::{HubState, Tenancy};
use cc_screen_protocol::SessionInfo;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::COOKIE;
use tokio_tungstenite::tungstenite::http::HeaderValue;

fn sess(name: &str) -> SessionInfo {
    SessionInfo {
        name: name.into(),
        tool: "shell".into(),
        short: name.into(),
        attached: false,
        activity: 0,
        last_input_at: 0,
        busy_since: 0,
        busy_until: 0,
        preview: String::new(),
        waiting: false,
        skip_permissions: None,
        cwd: String::new(),
        machine: String::new(),
        headline: None,
        detail: None,
        color: None,
        label: None,
    }
}

/// Build a multi-tenant hub over a fresh temp SQLite db with two users, each
/// owning an online (fake) agent labelled "laptop". Returns the base URL.
async fn start_multi_tenant_hub() -> String {
    start_hub_with(&["claude-a"], &["claude-b"]).await.0
}

/// As above, but with explicit session lists per tenant (so a sharing test can
/// give Alice a second session that a *session* share must NOT expose). Also
/// hands back the store, so a test can seed rows (e.g. a squeezed plan).
/// Both machines carry the SAME label "laptop" — the cross-tenant label collision.
async fn start_hub_with(alice_sessions: &[&str], bob_sessions: &[&str]) -> (String, Arc<SqliteStore>) {
    start_hub_labeled("laptop", alice_sessions, "laptop", bob_sessions).await
}

/// The general harness: two tenants with per-tenant machine labels + session
/// lists. Distinct labels let a test name *specifically* the other tenant's
/// machine (`?machine=alice-box`) and assert the refusal (0042 Stream A).
async fn start_hub_labeled(
    alice_label: &str,
    alice_sessions: &[&str],
    bob_label: &str,
    bob_sessions: &[&str],
) -> (String, Arc<SqliteStore>) {
    let (base, store, _maildir) = start_hub_full(alice_label, alice_sessions, bob_label, bob_sessions, true).await;
    (base, store)
}

/// The same harness with the mailer switchable, handing back the **maildir** as
/// well (proposal 0073 E1). `mail = true` is the capture transport every other
/// test already runs on; `mail = false` builds the hub a self-hoster gets —
/// [`Mailer::disabled`], no transport at all — and still returns the directory
/// the capture transport *would* have written to, so a test can prove nothing was
/// ever written there. Added beside `start_hub_labeled` rather than replacing it
/// so no existing caller has to grow a third binding it does not use.
async fn start_hub_full(
    alice_label: &str,
    alice_sessions: &[&str],
    bob_label: &str,
    bob_sessions: &[&str],
    mail: bool,
) -> (String, Arc<SqliteStore>, PathBuf) {
    let tmp = std::env::temp_dir().join(format!("ccr-hub-mt-{}-{}", std::process::id(), now_nanos()));
    let _ = std::fs::create_dir_all(&tmp);
    let store = SqliteStore::connect(&format!("sqlite://{}/hub.db", tmp.display()))
        .await
        .expect("open store");
    let alice = store.create_user("alice@x.com", "alicepass1-long").await.unwrap();
    let bob = store.create_user("bob@x.com", "bobpass1234-long").await.unwrap();
    let (_atok, alice_agent) = store.upsert_agent(&alice, alice_label).await.unwrap();
    let (_btok, bob_agent) = store.upsert_agent(&bob, bob_label).await.unwrap();

    let registry = Registry::new();
    // Register both agents online with a dummy uplink channel + their sessions.
    let (txa, _rxa) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&alice_agent, &alice, alice_label, "a.local", vec![], Vec::new(), txa)
        .set_sessions(alice_sessions.iter().map(|n| sess(n)).collect());
    let (txb, _rxb) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&bob_agent, &bob, bob_label, "b.local", vec![], Vec::new(), txb)
        .set_sessions(bob_sessions.iter().map(|n| sess(n)).collect());
    // Keep the fake agents' uplink receivers alive for the life of the hub so a
    // relayed Attach frame doesn't fail the send (the WS still upgrades regardless).
    std::mem::forget((_rxa, _rxb));

    let store = Arc::new(store);
    let maildir = tmp.join("mail");
    let mailer = if mail {
        cc_screen_hub::mailer::Mailer::capturing(maildir.clone())
    } else {
        cc_screen_hub::mailer::Mailer::disabled()
    };
    let hub = HubState {
        registry,
        agent_tokens: Arc::new(Default::default()),
        allow_open_uplink: false,
        // No shared secret — identity comes from the user store.
        client_auth: Auth::new(None, None, [3u8; 32]),
        origin: cc_screen_auth::OriginPolicy::default(),
        login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
        config_dir: tmp.clone(),
        push: Arc::new(cc_screen_push::Push::new(&tmp)),
        bulk: Default::default(),
        summary: Arc::new(cc_screen_hub::summarizer::Summarizer::disabled()),
        // Capture transport (proposal 0073 A3): every message this hub emits
        // lands as an `.eml` file under `<tmp>/mail`, so the invite tests assert
        // on real message bodies with no network and no SMTP container. The
        // `mail = false` hub takes `Mailer::disabled()` instead — the same hub a
        // self-hoster who configures no transport runs.
        mailer: Arc::new(mailer),
        tenancy: Tenancy::Multi(store.clone()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, cc_screen_hub::build_router(hub)).await.unwrap() });
    (format!("{addr}"), store, maildir)
}

/// The default two-tenant hub plus its maildir (proposal 0073 E1).
async fn start_hub_capturing() -> (String, Arc<SqliteStore>, PathBuf) {
    start_hub_full("laptop", &["claude-a"], "laptop", &["claude-b"], true).await
}

/// The self-hoster's hub: identical in every way except that it has **no mail
/// transport** (proposal 0073 E1). The returned path is the maildir that would
/// have been used, and must stay empty.
async fn start_hub_no_mail() -> (String, Arc<SqliteStore>, PathBuf) {
    start_hub_full("laptop", &["claude-a"], "laptop", &["claude-b"], false).await
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

/// Session names visible to a cookie via GET /api/sessions, sorted.
async fn session_names(client: &reqwest::Client, base: &str, cookie: &str) -> Vec<String> {
    let mut v: Vec<String> = client
        .get(format!("http://{base}/api/sessions"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
        .json::<Vec<SessionInfo>>()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    v.sort();
    v
}

/// GET a cookie'd JSON endpoint as a generic Value.
async fn get_json(client: &reqwest::Client, base: &str, cookie: &str, path: &str) -> serde_json::Value {
    client
        .get(format!("http://{base}{path}"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// POST a cookie'd JSON body; returns the response for status/body assertions.
async fn post_json(
    client: &reqwest::Client,
    base: &str,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    client
        .post(format!("http://{base}{path}"))
        .header(reqwest::header::COOKIE, cookie)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Does this cookie complete the WS attach handshake (101) for `session`?
async fn can_attach(base: &str, cookie: &str, session: &str) -> bool {
    let mut req = format!("ws://{base}/api/ws?session={session}").into_client_request().unwrap();
    req.headers_mut().insert(COOKIE, HeaderValue::from_str(cookie).unwrap());
    tokio_tungstenite::connect_async(req).await.is_ok()
}

/// Log in and return the `ccs_session=...` cookie pair for subsequent requests.
async fn login(client: &reqwest::Client, base: &str, email: &str, password: &str) -> String {
    let resp = client
        .post(format!("http://{base}/api/login"))
        .json(&serde_json::json!({ "email": email, "secret": password }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "login {email} should succeed");
    let set = resp.headers().get(reqwest::header::SET_COOKIE).expect("Set-Cookie").to_str().unwrap();
    set.split(';').next().unwrap().to_string() // "ccs_session=<...>"
}

#[tokio::test]
async fn tenants_are_isolated_end_to_end() {
    let base = start_multi_tenant_hub().await;
    let client = reqwest::Client::new();

    // ── unauthenticated is refused on a gated route ────────────────────────────
    let anon = client.get(format!("http://{base}/api/sessions")).send().await.unwrap();
    assert_eq!(anon.status(), reqwest::StatusCode::UNAUTHORIZED, "no cookie ⇒ 401");

    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // ── /api/sessions is scoped: each tenant sees only their own session ───────
    let list = |cookie: &str| {
        let client = client.clone();
        let base = base.clone();
        let cookie = cookie.to_string();
        async move {
            client.get(format!("http://{base}/api/sessions")).header(reqwest::header::COOKIE, cookie)
                .send().await.unwrap().json::<Vec<SessionInfo>>().await.unwrap()
        }
    };
    let alice_sessions = list(&alice).await;
    let bob_sessions = list(&bob).await;
    assert_eq!(alice_sessions.len(), 1);
    assert_eq!(alice_sessions[0].name, "claude-a", "alice sees only her session");
    assert_eq!(bob_sessions.len(), 1);
    assert_eq!(bob_sessions[0].name, "claude-b", "bob sees only his session");

    // ── cross-tenant control is refused: alice cannot reach bob's session ──────
    // Machine-less + bob's session name ⇒ resolve finds no agent in alice's scope.
    let cross = client
        .post(format!("http://{base}/api/clear-history"))
        .header(reqwest::header::COOKIE, &alice)
        .json(&serde_json::json!({ "session": "claude-b" }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), reqwest::StatusCode::NOT_FOUND, "alice can't control bob's session");

    // ── cross-tenant attach is refused at the WS handshake (no 101) ────────────
    let mut req = format!("ws://{base}/api/ws?session=claude-b").into_client_request().unwrap();
    req.headers_mut().insert(COOKIE, HeaderValue::from_str(&alice).unwrap());
    assert!(
        tokio_tungstenite::connect_async(req).await.is_err(),
        "alice attaching bob's session must be rejected before the upgrade"
    );

    // ── sanity: alice CAN attach her own session (the relay path still works) ──
    let mut ok_req = format!("ws://{base}/api/ws?session=claude-a").into_client_request().unwrap();
    ok_req.headers_mut().insert(COOKIE, HeaderValue::from_str(&alice).unwrap());
    assert!(
        tokio_tungstenite::connect_async(ok_req).await.is_ok(),
        "alice attaching her own session succeeds"
    );
}

// ── Sharing end-to-end (proposals 0039/0040/0041) ────────────────────────────

/// The headline flow: Alice agent-shares her box to Bob; Bob accepts via his
/// inbox; he then sees + can attach Alice's sessions; Alice revokes; access ends.
#[tokio::test]
async fn agent_share_invite_accept_revoke_end_to_end() {
    // Alice has two sessions so "agent share ⇒ sees them all" is meaningful.
    let (base, _store) = start_hub_with(&["claude-a", "claude-a2"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // Baseline isolation: Bob sees only his own; can't attach Alice's.
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"]);
    assert!(!can_attach(&base, &bob, "claude-a").await, "no grant ⇒ no attach");

    // Alice invites Bob to her whole machine.
    let created = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop" })).await;
    assert!(created.status().is_success(), "create invite");
    let invite_id = created.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();

    // It's pending: still no access until Bob accepts.
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"], "pending ≠ grant");

    // Bob's inbox shows exactly this invite, from Alice.
    let inbox = get_json(&client, &base, &bob, "/api/shares/inbox").await;
    let arr = inbox.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["inviterEmail"], "alice@x.com");
    assert_eq!(arr[0]["machine"], "laptop");

    // A non-grantee can't accept it (404, no state change); the grantee can.
    assert_eq!(
        post_json(&client, &base, &alice, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status(),
        reqwest::StatusCode::NOT_FOUND,
        "inviter cannot accept"
    );
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status().is_success());

    // Now Bob sees Alice's sessions (both) plus his own, and can attach one.
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-a", "claude-a2", "claude-b"]);
    assert!(can_attach(&base, &bob, "claude-a").await, "agent grant ⇒ attach works");
    assert_eq!(get_json(&client, &base, &bob, "/api/shares/received").await.as_array().unwrap().len(), 1);

    // Alice's outbox shows it accepted; she revokes.
    let outbox = get_json(&client, &base, &alice, "/api/shares/outbox").await;
    assert_eq!(outbox[0]["status"], "accepted");
    assert!(post_json(&client, &base, &alice, &format!("/api/shares/{invite_id}/revoke"), serde_json::json!({})).await.status().is_success());

    // Access ends on the next request; a second accept is a 409 (terminal).
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"], "revoke removes access");
    assert!(!can_attach(&base, &bob, "claude-a").await);
    assert_eq!(
        post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status(),
        reqwest::StatusCode::CONFLICT,
        "accepting a revoked invite is a conflict"
    );
}

/// A session-share grantee reaches the machine's **file surface** — and still
/// cannot run lifecycle ops on it.
///
/// The gate for files is `resolve_browsable` / `may_browse_agent`, not
/// `may_use_agent`. The reason is that the session grant already carries attach,
/// attach is keyboard input (0014 removed view-only), and the far end is an
/// assistant launched with `--dangerously-skip-permissions` — so the grantee can
/// already have it print or edit any file under the owner's `$HOME`. Gating the
/// browser on `may_use_agent` withheld nothing and answered "no online machine
/// for that request" about a machine that was online, which is what this test
/// exists to keep from coming back.
///
/// The fake agents in this harness never reply, so a routed op ends in 504 after
/// `REQUEST_TIMEOUT`. That is the assertion: **anything but 404** means the hub
/// authorized the request and tried to relay it. Both probes run concurrently so
/// the timeout is paid once.
#[tokio::test]
async fn session_share_reaches_files_but_not_lifecycle() {
    let (base, _store) = start_hub_labeled("alice-box", &["claude-a", "claude-a2"], "bob-box", &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // Alice shares ONE session; bob accepts.
    let created = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "alice-box", "session": "claude-a" })).await;
    let invite_id = created.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status().is_success());

    let nf = reqwest::StatusCode::NOT_FOUND;
    // The file surface: reading a file, and listing a folder the session's cwd
    // does not name. Neither carries `?session=`, which is exactly the shape the
    // PWA sends (`readFile` has no session parameter at all) and exactly what
    // used to 404.
    let (read, dirs) = tokio::join!(
        get_resp(&client, &base, &bob, "/api/file/read?machine=alice-box&path=/etc/hostname"),
        get_resp(&client, &base, &bob, "/api/dirs?machine=alice-box"),
    );
    assert_ne!(read.status(), nf, "a session grantee must reach file/read");
    assert_ne!(dirs.status(), nf, "…and the folder listing behind the tree");

    // But lifecycle is unchanged: he may not create or restore on alice's box.
    let create = post_json(&client, &base, &bob, "/api/session?machine=alice-box",
        serde_json::json!({ "name": "bobs-session", "tool": "shell", "dir": "/tmp" })).await;
    assert_eq!(create.status(), nf, "a session grant must not create sessions");
    assert_eq!(
        get_resp(&client, &base, &bob, "/api/sessions/restorable?machine=alice-box").await.status(),
        nf,
        "…nor reach restore",
    );

    // And a stranger still gets nothing — the widening is grant-scoped, not global.
    let carol = signup_ok(&client, &base, "carol@x.com", "carolpass12-long", "9.9.9.9").await;
    assert_eq!(
        get_resp(&client, &base, &carol, "/api/file/read?machine=alice-box&path=/etc/hostname").await.status(),
        nf,
        "no grant, no files",
    );
}

/// A *session* share exposes only the named session (not the rest of the box);
/// decline blocks it; a re-offer + accept grants it; Leave gives it back.
#[tokio::test]
async fn session_share_decline_reoffer_and_leave() {
    let (base, _store) = start_hub_with(&["claude-a", "claude-a2"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // Alice shares ONLY claude-a (a session), not the machine.
    let created = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop", "session": "claude-a" })).await;
    let invite_id = created.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();

    // Bob declines → no access, inbox empties.
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/decline"), serde_json::json!({})).await.status().is_success());
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"]);
    assert!(get_json(&client, &base, &bob, "/api/shares/inbox").await.as_array().unwrap().is_empty());

    // Alice re-offers the same session (terminal → pending again, same row).
    let reoffer = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop", "session": "claude-a" })).await;
    let reoffer_id = reoffer.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(reoffer_id, invite_id, "re-invite upserts the one row");

    // Bob accepts → sees ONLY claude-a added (NOT claude-a2), can attach it.
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status().is_success());
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-a", "claude-b"], "session grant is scoped");
    assert!(can_attach(&base, &bob, "claude-a").await);
    assert!(!can_attach(&base, &bob, "claude-a2").await, "the un-shared session stays hidden");

    // Bob leaves the share (grantee-side) → access gone.
    let received = get_json(&client, &base, &bob, "/api/shares/received").await;
    let grant_id = received[0]["id"].as_str().unwrap().to_string();
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/received/{grant_id}/leave"), serde_json::json!({})).await.status().is_success());
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"], "leave removes access");
}

// ── Proposal 0056 (in-product onboarding) + 0053 Part E (security gate) ───────

/// POST /api/signup with a per-test X-Forwarded-For (the signup throttle is a
/// process-wide per-source window — distinct sources keep tests independent).
async fn signup_req(
    client: &reqwest::Client,
    base: &str,
    email: &str,
    password: &str,
    source: &str,
) -> reqwest::Response {
    client
        .post(format!("http://{base}/api/signup"))
        .header("x-forwarded-for", source)
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()
        .await
        .unwrap()
}

/// Signup expected to succeed; returns the minted session cookie.
async fn signup_ok(client: &reqwest::Client, base: &str, email: &str, password: &str, source: &str) -> String {
    let resp = signup_req(client, base, email, password, source).await;
    assert!(resp.status().is_success(), "signup {email} should succeed ({})", resp.status());
    let set = resp.headers().get(reqwest::header::SET_COOKIE).expect("Set-Cookie").to_str().unwrap();
    set.split(';').next().unwrap().to_string()
}

/// Part C: inviting an email with no account creates a pending email invite
/// (uniform response + link), which lands in the inbox on signup and walks the
/// normal accept → grant path. A revoke before signup means nothing attaches.
#[tokio::test]
async fn email_invite_signup_lands_in_inbox() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    // Invite a ghost address → 200 with the unified shape + a copyable link.
    let created = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost@x.com", "machine": "laptop" })).await;
    assert!(created.status().is_success(), "unknown email no longer 404s");
    let j = created.json::<serde_json::Value>().await.unwrap();
    assert_eq!(j["status"], "pending");
    let invite_url = j["invite_url"].as_str().expect("invite_url").to_string();
    let token = invite_url.rsplit('/').next().unwrap().to_string();
    assert!(invite_url.contains("/invite/") && !token.is_empty());

    // The outbox shows it as an "invited" row addressed to the email.
    let outbox = get_json(&client, &base, &alice, "/api/shares/outbox").await;
    let invited: Vec<_> = outbox.as_array().unwrap().iter().filter(|r| r["status"] == "invited").collect();
    assert_eq!(invited.len(), 1);
    assert_eq!(invited[0]["granteeEmail"], "ghost@x.com");

    // The public invite-link read resolves for the token holder (no cookie).
    let info = client.get(format!("http://{base}/api/invite/{token}")).send().await.unwrap();
    assert!(info.status().is_success());
    let info = info.json::<serde_json::Value>().await.unwrap();
    assert_eq!(info["email"], "ghost@x.com");
    assert_eq!(info["inviterEmail"], "alice@x.com");
    assert_eq!(info["kind"], "agent");
    // A bogus token 404s.
    let bogus = client.get(format!("http://{base}/api/invite/not-a-token")).send().await.unwrap();
    assert_eq!(bogus.status(), reqwest::StatusCode::NOT_FOUND);

    // Ghost signs up → the invite is waiting in the inbox on the first poll.
    let ghost = signup_ok(&client, &base, "ghost@x.com", "ghostpass12345", "10.56.0.1").await;
    let inbox = get_json(&client, &base, &ghost, "/api/shares/inbox").await;
    let arr = inbox.as_array().unwrap();
    assert_eq!(arr.len(), 1, "email invite converted into a pending 0040 invite");
    assert_eq!(arr[0]["inviterEmail"], "alice@x.com");
    let invite_id = arr[0]["id"].as_str().unwrap().to_string();

    // Accepting walks the untouched 0039 path — ghost now sees alice's session.
    assert!(post_json(&client, &base, &ghost, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status().is_success());
    let names = session_names(&client, &base, &ghost).await;
    assert!(names.contains(&"claude-a".to_string()), "got: {names:?}");

    // Scenario 2: revoke BEFORE signup → nothing attaches for that address.
    let created2 = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost2@x.com", "machine": "laptop" })).await;
    let j2 = created2.json::<serde_json::Value>().await.unwrap();
    let id2 = j2["id"].as_str().unwrap().to_string();
    assert!(post_json(&client, &base, &alice, &format!("/api/shares/{id2}/revoke"), serde_json::json!({})).await.status().is_success(),
        "outbox Cancel works on an email invite via the revoke fall-through");
    let ghost2 = signup_ok(&client, &base, "ghost2@x.com", "ghost2pass1234", "10.56.0.2").await;
    let inbox2 = get_json(&client, &base, &ghost2, "/api/shares/inbox").await;
    assert!(inbox2.as_array().unwrap().is_empty(), "revoked pre-signup ⇒ nothing attaches");
}

/// Part C2 / [0042]: creating a share to a known vs unknown email answers with
/// the same status and the same JSON shape — no account-existence oracle.
#[tokio::test]
async fn share_create_does_not_disclose_accounts() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    let known = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop" })).await;
    let unknown = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "nobody-here@x.com", "machine": "laptop" })).await;
    assert_eq!(known.status(), unknown.status(), "identical status");

    let kj = known.json::<serde_json::Value>().await.unwrap();
    let uj = unknown.json::<serde_json::Value>().await.unwrap();
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(keys(&kj), keys(&uj), "identical body shape (keys)");
    assert_eq!(kj["status"], uj["status"], "both report pending");
    assert!(kj["invite_url"].as_str().unwrap().contains("/invite/"));
    assert!(uj["invite_url"].as_str().unwrap().contains("/invite/"));
}

/// Part B: the machine cap answers 402 with the human message; unlinking frees
/// a slot and re-approving the same (still-pending) code succeeds.
#[tokio::test]
async fn machine_cap_answers_402_with_message() {
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    // Squeeze alice onto a tiny plan (1 machine — she already has "laptop").
    sqlx::query("INSERT INTO plan_limits (plan, max_agents, max_concurrent_sessions) VALUES ('tiny56', 1, 1)")
        .execute(store.pool())
        .await
        .unwrap();
    store.set_plan("alice@x.com", "tiny56").await.unwrap();

    // /api/me reports the squeezed plan facts (proposal 0056 B1).
    let me = get_json(&client, &base, &alice, "/api/me").await;
    assert_eq!(me["plan"]["name"], "tiny56");
    assert_eq!(me["plan"]["maxAgents"], 1);
    assert_eq!(me["plan"]["agents"], 1);

    // A NEW machine's device code → approve is refused with 402 + the message.
    let code = client
        .post(format!("http://{base}/api/device/code"))
        .json(&serde_json::json!({ "device_id": "d-56", "machine_id": "newbox" }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let user_code = code["user_code"].as_str().unwrap().to_string();
    let denied = post_json(&client, &base, &alice, "/api/device/approve",
        serde_json::json!({ "user_code": user_code })).await;
    assert_eq!(denied.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let msg = denied.text().await.unwrap();
    assert!(msg.contains("Machine limit reached"), "got: {msg}");

    // Unlink the existing machine → the SAME pending code now approves fine.
    let agents = get_json(&client, &base, &alice, "/api/agents").await;
    let agent_id = agents[0]["agentId"].as_str().unwrap().to_string();
    let unlinked = post_json(&client, &base, &alice, "/api/agents/unlink",
        serde_json::json!({ "agent_id": agent_id })).await;
    assert!(unlinked.status().is_success());
    let approved = post_json(&client, &base, &alice, "/api/device/approve",
        serde_json::json!({ "user_code": user_code })).await;
    assert!(approved.status().is_success(), "re-approve after unlink ({})", approved.status());
}

/// Part B: the concurrent-session cap answers 402 from POST /api/session.
#[tokio::test]
async fn session_cap_answers_402() {
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    sqlx::query("INSERT INTO plan_limits (plan, max_agents, max_concurrent_sessions) VALUES ('tiny56s', 10, 1)")
        .execute(store.pool())
        .await
        .unwrap();
    store.set_plan("alice@x.com", "tiny56s").await.unwrap();

    // Alice already runs 1 session (claude-a) — creating another hits the cap.
    let denied = post_json(&client, &base, &alice, "/api/session",
        serde_json::json!({ "tool": "cc", "name": "second", "dir": "/tmp" })).await;
    assert_eq!(denied.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    let msg = denied.text().await.unwrap();
    assert!(msg.contains("Session limit reached"), "got: {msg}");
}

/// 0053 Part E item 1: the duplicate-email signup path is indistinguishable
/// (status + body) from any other server-side signup failure, and login answers
/// identically for unknown emails and wrong passwords.
#[tokio::test]
async fn signup_no_enumeration() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();

    // Duplicate email (alice exists) → the one generic failure answer.
    let dup = signup_req(&client, &base, "alice@x.com", "a-valid-password", "10.56.1.1").await;
    assert_eq!(dup.status(), reqwest::StatusCode::CONFLICT);
    let dup_body = dup.json::<serde_json::Value>().await.unwrap();
    let msg = dup_body["error"].as_str().unwrap().to_lowercase();
    assert!(
        !msg.contains("duplicate") && !msg.contains("exists") && !msg.contains("taken"),
        "must not confirm the account exists: {msg}"
    );
    // A second, independent duplicate answers byte-identically.
    let dup2 = signup_req(&client, &base, "bob@x.com", "a-valid-password", "10.56.1.2").await;
    assert_eq!(dup2.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(dup_body, dup2.json::<serde_json::Value>().await.unwrap(), "one generic body for all failures");

    // Login: unknown email and wrong password answer with the same status/body.
    let bad_pw = client.post(format!("http://{base}/api/login"))
        .json(&serde_json::json!({ "email": "alice@x.com", "secret": "wrong-password" }))
        .send().await.unwrap();
    let unknown = client.post(format!("http://{base}/api/login"))
        .json(&serde_json::json!({ "email": "who@x.com", "secret": "wrong-password" }))
        .send().await.unwrap();
    assert_eq!(bad_pw.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status(), bad_pw.status());
    assert_eq!(bad_pw.text().await.unwrap(), unknown.text().await.unwrap());
}

/// 0053 Part E item 2: a burst of signups from one source is rate-limited (429)
/// while a normal journey (a signup or two) is untouched.
#[tokio::test]
async fn signup_throttled() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let source = "10.56.2.99"; // dedicated to this test — the window is per-source

    let mut saw_429 = false;
    for i in 0..8 {
        let r = signup_req(&client, &base, &format!("burst{i}@x.com"), "burst-password-12", source).await;
        if i < 5 {
            assert_ne!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS, "attempt {i} within the window");
        } else if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
        }
    }
    assert!(saw_429, "a >5/min burst from one source must hit 429");

    // A different source is unaffected.
    let other = signup_req(&client, &base, "calm@x.com", "calm-password-12", "10.56.2.100").await;
    assert!(other.status().is_success(), "other sources keep signing up");
}

/// 0058 A3: creating shares is plan-gated (402 for a no-share plan), receiving is
/// not (the gated user still sees + accepts invites), and a Pro/beta plan shares
/// as before.
#[tokio::test]
async fn share_creation_is_plan_gated_receiving_is_not() {
    use sqlx::Row;
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // `free` defaults to can_create_shares=1 (the migration is dark-safe), so seed
    // an explicit no-share plan and put alice on it.
    sqlx::query(
        "INSERT INTO plan_limits (plan, max_agents, max_concurrent_sessions, can_create_shares) \
         VALUES ('noshare58', 2, 5, 0)",
    )
    .execute(store.pool())
    .await
    .unwrap();
    store.set_plan("alice@x.com", "noshare58").await.unwrap();

    // Alice can't create a share → 402 with the message; no artifact created.
    let denied = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop" })).await;
    assert_eq!(denied.status(), reqwest::StatusCode::PAYMENT_REQUIRED);
    assert!(denied.text().await.unwrap().contains("Pro feature"));
    let count = |t: &str, s: &Arc<SqliteStore>| {
        let q = format!("SELECT count(*) AS n FROM {t}");
        let pool = s.pool().clone();
        async move { sqlx::query(&q).fetch_one(&pool).await.unwrap().try_get::<i64, _>("n").unwrap() }
    };
    assert_eq!(count("share_invites", &store).await, 0, "no share_invites row");
    assert_eq!(count("email_invites", &store).await, 0, "no email_invites row");

    // Receiving is NOT gated: bob (default free) invites alice; she sees + accepts.
    let created = post_json(&client, &base, &bob, "/api/shares",
        serde_json::json!({ "grantee_email": "alice@x.com", "machine": "laptop" })).await;
    assert!(created.status().is_success(), "bob (free) creates fine ({})", created.status());
    let inbox = get_json(&client, &base, &alice, "/api/shares/inbox").await;
    assert_eq!(inbox.as_array().unwrap().len(), 1, "gated user still receives");
    let id = inbox[0]["id"].as_str().unwrap().to_string();
    let accepted = post_json(&client, &base, &alice, &format!("/api/shares/{id}/accept"), serde_json::json!({})).await;
    assert!(accepted.status().is_success(), "gated user can accept ({})", accepted.status());

    // On beta (can_create_shares=1) alice shares again.
    store.set_plan("alice@x.com", "beta").await.unwrap();
    let ok = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop" })).await;
    assert!(ok.status().is_success(), "beta can share ({})", ok.status());
}

/// 0058 acceptance 11 (graceful absence): with no STRIPE_* env, `/api/me` reports
/// `billing:false`, plan facts still resolve, and no subscription status leaks.
#[tokio::test]
async fn billing_absent_reports_false_and_plan_still_works() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let me = get_json(&client, &base, &alice, "/api/me").await;
    assert_eq!(me["billing"], serde_json::Value::Bool(false), "no Stripe env → billing:false");
    assert!(me["plan"]["name"].is_string(), "user plan still resolves");
    assert!(me["plan"].get("status").is_none(), "no subscription → status omitted");
    assert!(me["plan"].get("periodEnd").is_none(), "no subscription → periodEnd omitted");
}

/// 0053 Part E item 3: public signup requires a 12-character password.
#[tokio::test]
async fn password_min_12() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();

    let short = signup_req(&client, &base, "pwtest@x.com", "elevenchars", "10.56.3.1").await;
    assert_eq!(short.status(), reqwest::StatusCode::BAD_REQUEST);
    let body = short.json::<serde_json::Value>().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("12"), "hint names the minimum");

    let ok = signup_req(&client, &base, "pwtest@x.com", "twelve-chars", "10.56.3.2").await;
    assert!(ok.status().is_success(), "12 chars accepted ({})", ok.status());
}

// ── proposal 0060: the terminal-client sign-in wire contract ──────────────────

/// A client token from Bearer must light up the whole client surface with the
/// caller's own scope — and NOTHING else: not the other tenant's sessions, and
/// never as an uplink credential.
#[tokio::test]
async fn client_token_flow_end_to_end() {
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice_cookie = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    // Terminal side: request a code (unauthenticated, distinct path)…
    let code: serde_json::Value = client
        .post(format!("http://{base}/api/device/client/code"))
        .json(&serde_json::json!({ "label": "alice@testbox" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(code["verification_uri"].as_str().unwrap().ends_with("/activate"));

    // …browser side approves with the cookie; the response says WHAT (B6).
    let approve = post_json(
        &client,
        &base,
        &alice_cookie,
        "/api/device/approve",
        serde_json::json!({ "user_code": code["user_code"] }),
    )
    .await;
    assert!(approve.status().is_success(), "approve: {}", approve.status());
    let approved: serde_json::Value = approve.json().await.unwrap();
    assert_eq!(approved["kind"], "client");
    assert_eq!(approved["label"], "alice@testbox");

    // …terminal polls the token endpoint: one-time handover + the account email.
    let tok: serde_json::Value = client
        .post(format!("http://{base}/api/device/client/token"))
        .json(&serde_json::json!({ "device_code": code["device_code"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bearer = tok["client_token"].as_str().expect("client_token").to_string();
    assert_eq!(tok["email"], "alice@x.com");

    // The Bearer sees exactly alice's scope on REST…
    let names: Vec<String> = client
        .get(format!("http://{base}/api/sessions"))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap()
        .json::<Vec<SessionInfo>>()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["claude-a"], "alice's sessions only");

    // …and on the WS attach handshake (the same middleware gates both).
    let mut req = format!("ws://{base}/api/ws?session=claude-a").into_client_request().unwrap();
    req.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {bearer}")).unwrap(),
    );
    assert!(tokio_tungstenite::connect_async(req).await.is_ok(), "bearer WS attach upgrades");

    // Two-credential invariant, both directions: an *uplink* token presented as
    // client Bearer is refused…
    let alice_id = store.user_id_by_email("alice@x.com").await.unwrap();
    let (uplink, _aid) = store.upsert_agent(&alice_id, "laptop").await.unwrap();
    let r = client
        .get(format!("http://{base}/api/sessions"))
        .bearer_auth(&uplink)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED, "uplink token is not a client credential");
    // …and the client token never resolves as an uplink credential.
    assert!(store.resolve_agent("laptop", Some(&bearer)).await.is_none());

    // The account page lists the terminal client (cookie-authed)…
    let list = get_json(&client, &base, &alice_cookie, "/api/client-tokens").await;
    let row = &list.as_array().expect("array")[0];
    assert_eq!(row["label"], "alice@testbox");

    // …bob can't revoke it…
    let bob_cookie = login(&client, &base, "bob@x.com", "bobpass1234-long").await;
    let steal = post_json(
        &client,
        &base,
        &bob_cookie,
        "/api/client-tokens/delete",
        serde_json::json!({ "id": row["id"] }),
    )
    .await;
    assert_eq!(steal.status(), reqwest::StatusCode::NOT_FOUND, "owner-scoped revoke");

    // …and `ccs logout` (Bearer self-revoke) kills it immediately.
    let revoke = client
        .post(format!("http://{base}/api/client-tokens/revoke-self"))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success());
    let after = client
        .get(format!("http://{base}/api/sessions"))
        .bearer_auth(&bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), reqwest::StatusCode::UNAUTHORIZED, "revoked token 401s at once");
}

// ── Proposal 0042 Stream A/I: the full cross-tenant refusal surface ───────────

/// GET a cookie'd path; returns the response for status assertions.
async fn get_resp(client: &reqwest::Client, base: &str, cookie: &str, path: &str) -> reqwest::Response {
    client
        .get(format!("http://{base}{path}"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
}

/// PUT a cookie'd JSON body; returns the response.
async fn put_json(
    client: &reqwest::Client,
    base: &str,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    client
        .put(format!("http://{base}{path}"))
        .header(reqwest::header::COOKIE, cookie)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// Does this cookie complete the WS upgrade (101) for an arbitrary /api WS path?
async fn ws_upgrades(base: &str, cookie: &str, path_and_query: &str) -> bool {
    let mut req = format!("ws://{base}{path_and_query}").into_client_request().unwrap();
    req.headers_mut().insert(COOKIE, HeaderValue::from_str(cookie).unwrap());
    tokio_tungstenite::connect_async(req).await.is_ok()
}

/// The 0042 Stream A acceptance, widened to the FULL relay surface: tenant B —
/// fully authenticated, with an online agent of their own — is refused (404,
/// never 2xx, never a relayed effect) on every route that resolves an agent,
/// attacking by A's machine label (`?machine=`), by A's session name, and via
/// machine-less resolution. Distinct labels ("alice-box"/"bob-box") make the
/// attack target unambiguous: a 404 here is `resolve_scoped` refusing in B's
/// scope, before anything reaches A's agent.
#[tokio::test]
async fn cross_tenant_full_surface_is_refused() {
    let (base, _store) = start_hub_labeled("alice-box", &["claude-a"], "bob-box", &["claude-b"]).await;
    let client = reqwest::Client::new();
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;
    let nf = reqwest::StatusCode::NOT_FOUND;

    // Positive controls first: bob's own scope works, so every refusal below is
    // the boundary, not a broken harness.
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"]);
    assert!(can_attach(&base, &bob, "claude-b").await, "bob attaches his own session");
    assert!(ws_upgrades(&base, &bob, "/api/watch?machine=bob-box").await, "bob watches his own box");

    // ── GET routes: by A's machine label AND by A's session name ──────────────
    let gets: &[&str] = &[
        "/api/dirs?machine=alice-box",
        "/api/dirs?session=claude-a",
        "/api/dirs/search?machine=alice-box&q=src",
        "/api/files/search?machine=alice-box&q=main",
        "/api/files?machine=alice-box",
        "/api/files?session=claude-a",
        "/api/file/read?machine=alice-box&path=/etc/hostname",
        "/api/file/read?session=claude-a&path=/etc/hostname",
        "/api/session/root?machine=alice-box",
        "/api/session/root?session=claude-a",
        "/api/sessions/restorable?machine=alice-box",
        "/api/assistants/update?machine=alice-box",
        "/api/assistants/plan?machine=alice-box",
        // The bulk proxy (download) must refuse BEFORE any relay is opened.
        "/api/download?machine=alice-box&path=/etc/passwd",
        "/api/download?session=claude-a&path=/etc/passwd",
    ];
    for path in gets {
        let r = get_resp(&client, &base, &bob, path).await;
        assert_eq!(r.status(), nf, "GET {path} must be refused for tenant B");
    }

    // ── POST routes: lifecycle, control, file mutations ───────────────────────
    let posts: &[(&str, serde_json::Value)] = &[
        ("/api/session?machine=alice-box", serde_json::json!({ "tool": "cc", "name": "intruder", "dir": "/tmp" })),
        ("/api/session/delete", serde_json::json!({ "session": "claude-a" })),
        ("/api/session/delete?machine=alice-box", serde_json::json!({ "session": "claude-a" })),
        ("/api/session/color", serde_json::json!({ "session": "claude-a", "color": "red" })),
        ("/api/session/label", serde_json::json!({ "session": "claude-a", "label": "pwned" })),
        ("/api/sessions/restore?machine=alice-box", serde_json::json!({})),
        ("/api/key", serde_json::json!({ "session": "claude-a", "key": "enter" })),
        ("/api/key?machine=alice-box", serde_json::json!({ "session": "claude-a", "key": "enter" })),
        ("/api/paste", serde_json::json!({ "session": "claude-a", "text": "rm -rf /", "enter": true })),
        ("/api/clear-history", serde_json::json!({ "session": "claude-a" })),
        ("/api/file/write?machine=alice-box", serde_json::json!({ "path": "/tmp/x", "content": "hi" })),
        ("/api/file/delete?machine=alice-box", serde_json::json!({ "path": "/tmp/x" })),
        ("/api/mkdir?machine=alice-box", serde_json::json!({ "path": "/tmp/x" })),
        ("/api/rmdir?machine=alice-box", serde_json::json!({ "path": "/tmp/x" })),
        ("/api/rename?machine=alice-box", serde_json::json!({ "from": "/tmp/a", "to": "/tmp/b" })),
        ("/api/move?machine=alice-box", serde_json::json!({ "from": "/tmp/a", "to": "/tmp/b" })),
        ("/api/assistants/update?machine=alice-box", serde_json::json!({ "tools": [] })),
    ];
    for (path, body) in posts {
        let r = post_json(&client, &base, &bob, path, body.clone()).await;
        assert_eq!(r.status(), nf, "POST {path} must be refused for tenant B");
    }

    // ── The two WS bridges refuse at the handshake (no 101, nothing bridged) ──
    assert!(!ws_upgrades(&base, &bob, "/api/ws?session=claude-a").await, "terminal by A's session");
    assert!(!ws_upgrades(&base, &bob, "/api/ws?machine=alice-box&session=claude-a").await, "terminal by A's machine");
    assert!(!ws_upgrades(&base, &bob, "/api/watch?machine=alice-box").await, "fs-watch on A's machine");

    // ── /api/tools discloses nothing: A's machine answers exactly like a ──────
    // machine that does not exist (the documented `[]` contract, not a 404).
    let a_tools = get_resp(&client, &base, &bob, "/api/tools?machine=alice-box").await;
    assert_eq!(a_tools.status(), reqwest::StatusCode::OK);
    let a_tools: serde_json::Value = a_tools.json().await.unwrap();
    let ghost = get_json(&client, &base, &bob, "/api/tools?machine=no-such-box").await;
    assert_eq!(a_tools, ghost, "A's tools indistinguishable from an unknown machine");
    assert!(a_tools.as_array().unwrap().is_empty());

    // ── /api/machines never lists A's box for B ───────────────────────────────
    let machines = get_json(&client, &base, &bob, "/api/machines").await;
    let labels: Vec<&str> = machines.as_array().unwrap().iter().map(|m| m["machine"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["bob-box"], "B's machine list holds only his own");
}

/// 0049/0050 through the 0042 lens: assistant update/plan are OWNER-only. Even a
/// full agent-grant (the widest share there is) yields 403 — a share grants
/// *use*, never administration of someone else's host.
#[tokio::test]
async fn agent_grantee_cannot_administer_assistants() {
    let (base, _store) = start_hub_labeled("alice-box", &["claude-a"], "bob-box", &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // Alice agent-shares her whole box to Bob; Bob accepts.
    let created = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "alice-box" })).await;
    assert!(created.status().is_success());
    let invite_id = created.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    assert!(post_json(&client, &base, &bob, &format!("/api/shares/{invite_id}/accept"), serde_json::json!({})).await.status().is_success());

    // The grant works: Bob now resolves + lists Alice's box…
    let names = session_names(&client, &base, &bob).await;
    assert!(names.contains(&"claude-a".to_string()), "grant active: {names:?}");

    // …but administration stays 403 (resolve succeeds, ownership fails).
    let fb = reqwest::StatusCode::FORBIDDEN;
    let st = get_resp(&client, &base, &bob, "/api/assistants/update?machine=alice-box").await;
    assert_eq!(st.status(), fb, "status read is owner-only");
    let up = post_json(&client, &base, &bob, "/api/assistants/update?machine=alice-box",
        serde_json::json!({ "tools": [] })).await;
    assert_eq!(up.status(), fb, "update is owner-only");
    let plan = get_resp(&client, &base, &bob, "/api/assistants/plan?machine=alice-box").await;
    assert_eq!(plan.status(), fb, "install plan is owner-only");
    // The owner herself passes the ownership gate (the fake agent advertises no
    // caps, so she gets the 501 capability answer — proving 403 was the tenant
    // gate, not a dead route).
    let own = get_resp(&client, &base, &alice, "/api/assistants/update?machine=alice-box").await;
    assert_eq!(own.status(), reqwest::StatusCode::NOT_IMPLEMENTED);
}

/// Org routes are membership-scoped (proposal 0063 B1, under the 0042 bar):
/// a tenant outside the org gets 404 everywhere — existence is never disclosed
/// — and cannot act on another org's invites or members.
#[tokio::test]
async fn org_routes_are_tenant_scoped() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;
    let nf = reqwest::StatusCode::NOT_FOUND;

    // Alice creates a team; sanity: her own org surface answers.
    let created = post_json(&client, &base, &alice, "/api/orgs", serde_json::json!({ "name": "team-a" })).await;
    assert!(created.status().is_success(), "org create: {}", created.status());
    let mine = get_resp(&client, &base, &alice, "/api/orgs/mine").await;
    assert!(mine.status().is_success());
    assert_eq!(mine.json::<serde_json::Value>().await.unwrap()["myRole"], "owner");
    assert!(get_resp(&client, &base, &alice, "/api/orgs/audit").await.status().is_success(), "owner reads the audit log");

    // Bob (no org): every org route is a uniform 404 — no existence oracle.
    assert_eq!(get_resp(&client, &base, &bob, "/api/orgs/mine").await.status(), nf);
    assert_eq!(get_resp(&client, &base, &bob, "/api/orgs/audit").await.status(), nf);
    assert_eq!(get_resp(&client, &base, &bob, "/api/orgs/invites").await.status(), nf);
    assert_eq!(
        post_json(&client, &base, &bob, "/api/orgs/invites", serde_json::json!({ "email": "x@x.com" })).await.status(),
        nf
    );
    assert_eq!(post_json(&client, &base, &bob, "/api/orgs/leave", serde_json::json!({})).await.status(), nf);
    assert_eq!(
        post_json(&client, &base, &bob, "/api/orgs/members/some-user/role", serde_json::json!({ "role": "admin" })).await.status(),
        nf
    );
    assert_eq!(
        post_json(&client, &base, &bob, "/api/orgs/members/some-user/remove", serde_json::json!({})).await.status(),
        nf
    );
    assert_eq!(
        post_json(&client, &base, &bob, "/api/orgs/machines/some-agent/visibility", serde_json::json!({ "visible": false })).await.status(),
        nf
    );

    // Alice invites carol; Bob can neither accept, decline, nor revoke it.
    let inv = post_json(&client, &base, &alice, "/api/orgs/invites", serde_json::json!({ "email": "carol@x.com" })).await;
    assert!(inv.status().is_success(), "invite create: {}", inv.status());
    let inv_id = inv.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(post_json(&client, &base, &bob, &format!("/api/orgs/invites/{inv_id}/accept"), serde_json::json!({})).await.status(), nf, "not the addressee");
    assert_eq!(post_json(&client, &base, &bob, &format!("/api/orgs/invites/{inv_id}/decline"), serde_json::json!({})).await.status(), nf);
    assert_eq!(post_json(&client, &base, &bob, &format!("/api/orgs/invites/{inv_id}/revoke"), serde_json::json!({})).await.status(), nf, "no org, no revoke");

    // The invite survived every foreign action: still pending in Alice's outbox.
    let outbox = get_json(&client, &base, &alice, "/api/orgs/invites").await;
    let pending: Vec<_> = outbox.as_array().unwrap().iter().filter(|i| i["status"] == "pending").collect();
    assert_eq!(pending.len(), 1, "invite untouched by the refused actions");
}

/// Proposal 0073 B1/B2: a delivery attempt that a **re-invite or a revoke**
/// superseded writes nothing — the receipt in the outbox can only ever come from
/// the attempt that still owns the token. Also pins the two halves of the
/// no-oracle contract this proposal must not move: the create response keeps its
/// exact `{id, status, invite_url}` shape (an `emailed` field would be a fresh
/// account-existence oracle), while `delivery` is read back from the outbox,
/// which is inviter-scoped and asymmetric by design.
#[tokio::test]
async fn superseded_invite_send_writes_nothing() {
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let token_of = |url: &str| url.rsplit('/').next().unwrap_or_default().to_string();

    // ── the share path: the receipt lives on the email-invite row ──────────────
    let s1 = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost@x.com", "machine": "laptop" })).await;
    assert!(s1.status().is_success(), "share create: {}", s1.status());
    let s1: serde_json::Value = s1.json().await.unwrap();
    let (share_id, share_tok1) = (s1["id"].as_str().unwrap().to_string(), token_of(s1["invite_url"].as_str().unwrap()));
    // A re-offer keeps the row and mints a fresh token: attempt one is dead.
    let s2 = post_json(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost@x.com", "machine": "laptop" })).await;
    let s2: serde_json::Value = s2.json().await.unwrap();
    assert_eq!(s2["id"], s1["id"], "same email-invite row");
    assert_ne!(token_of(s2["invite_url"].as_str().unwrap()), share_tok1, "fresh token");
    assert!(!store.email_invite_mark_sending(&share_id, &share_tok1).await, "superseded ⇒ no send");

    // ── the org path ──────────────────────────────────────────────────────────
    let created = post_json(&client, &base, &alice, "/api/orgs", serde_json::json!({ "name": "team-a" })).await;
    assert!(created.status().is_success(), "org create: {}", created.status());

    let inv = post_json(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "ghost@x.com" })).await;
    assert!(inv.status().is_success(), "invite create: {}", inv.status());
    let inv: serde_json::Value = inv.json().await.unwrap();
    let obj = inv.as_object().unwrap();
    assert_eq!(obj.len(), 3, "the create response shape does not move: {inv}");
    for k in ["id", "status", "invite_url"] {
        assert!(obj.contains_key(k), "missing {k} in {inv}");
    }
    assert!(obj.get("emailed").is_none(), "an `emailed` field would be an account-existence oracle");
    let (id, tok1) = (inv["id"].as_str().unwrap().to_string(), token_of(inv["invite_url"].as_str().unwrap()));

    // Re-invite: same row, fresh token — whatever attempt one was doing is void.
    let again = post_json(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "ghost@x.com" })).await;
    let again: serde_json::Value = again.json().await.unwrap();
    assert_eq!(again["id"], inv["id"]);
    let tok2 = token_of(again["invite_url"].as_str().unwrap());
    assert_ne!(tok2, tok1, "a re-invite mints a fresh token");

    // The superseded attempt can neither claim the row nor stamp an outcome on it.
    assert!(!store.org_invite_mark_sending(&id, &tok1).await, "superseded ⇒ no send");
    store.org_invite_delivery_record(&id, &tok1, "failed").await;

    // The outbox exposes `delivery` for the UI — and never attempt one's verdict.
    let outbox = get_json(&client, &base, &alice, "/api/orgs/invites").await;
    let row = outbox.as_array().unwrap().iter().find(|r| r["id"] == inv["id"]).expect("invite in the outbox");
    assert!(row.get("delivery").is_some(), "the outbox carries `delivery` (null until a send is attempted)");
    assert_ne!(row["delivery"], serde_json::json!("failed"), "the superseded attempt wrote nothing");

    // A revoke inside the send window kills the live attempt's claim too.
    let rev = post_json(&client, &base, &alice, &format!("/api/orgs/invites/{id}/revoke"), serde_json::json!({})).await;
    assert!(rev.status().is_success(), "revoke: {}", rev.status());
    assert!(!store.org_invite_mark_sending(&id, &tok2).await, "a revoked invite is never mailed");
}

/// Favorites are per-tenant (the 0042 candidate-finding-#1 fix): what A saves,
/// B never sees, and each tenant round-trips their own list independently.
#[tokio::test]
async fn favorites_are_tenant_scoped() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;

    // Both start empty.
    assert!(get_json(&client, &base, &alice, "/api/favorites").await.as_array().unwrap().is_empty());
    assert!(get_json(&client, &base, &bob, "/api/favorites").await.as_array().unwrap().is_empty());

    // Alice saves a favorite…
    let put = put_json(&client, &base, &alice, "/api/favorites",
        serde_json::json!([{ "id": "f1", "text": "deploy the thing" }])).await;
    assert!(put.status().is_success(), "alice PUT: {}", put.status());

    // …she reads it back; Bob reads EMPTY (the leak this fix closes).
    let a1 = get_json(&client, &base, &alice, "/api/favorites").await;
    assert_eq!(a1.as_array().unwrap().len(), 1);
    assert_eq!(a1[0]["id"], "f1");
    assert!(
        get_json(&client, &base, &bob, "/api/favorites").await.as_array().unwrap().is_empty(),
        "tenant B must not see tenant A's favorites"
    );

    // Bob keeps his own list without touching Alice's.
    assert!(put_json(&client, &base, &bob, "/api/favorites",
        serde_json::json!([{ "id": "b1", "text": "bob's own" }])).await.status().is_success());
    let a2 = get_json(&client, &base, &alice, "/api/favorites").await;
    assert_eq!(a2.as_array().unwrap().len(), 1);
    assert_eq!(a2[0]["id"], "f1", "alice's list untouched by bob's write");
    let b2 = get_json(&client, &base, &bob, "/api/favorites").await;
    assert_eq!(b2.as_array().unwrap().len(), 1);
    assert_eq!(b2[0]["id"], "b1");
}

/// A client code redeemed through the *agent* poll endpoint (or vice versa) is
/// a uniform dead end — a pre-0060 hub's shape can never be tricked into
/// cross-kind handover.
#[tokio::test]
async fn device_flow_kinds_never_cross() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice_cookie = login(&client, &base, "alice@x.com", "alicepass1-long").await;

    let code: serde_json::Value = client
        .post(format!("http://{base}/api/device/client/code"))
        .json(&serde_json::json!({ "label": "x" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let approve = post_json(
        &client,
        &base,
        &alice_cookie,
        "/api/device/approve",
        serde_json::json!({ "user_code": code["user_code"] }),
    )
    .await;
    assert!(approve.status().is_success());

    // Redeeming the approved CLIENT code through the AGENT endpoint fails
    // uniformly ("expired_token") and hands over nothing…
    let cross = client
        .post(format!("http://{base}/api/device/token"))
        .json(&serde_json::json!({ "device_code": code["device_code"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = cross.json().await.unwrap();
    assert_eq!(body["error"], "expired_token");

    // …and the legitimate endpoint still delivers exactly once afterwards (the
    // cross-kind read didn't consume the row).
    let tok: serde_json::Value = client
        .post(format!("http://{base}/api/device/client/token"))
        .json(&serde_json::json!({ "device_code": code["device_code"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(tok["client_token"].is_string(), "own-kind redeem still works: {tok}");
}

// ── Proposal 0073 Part C — the abuse bounds that ship with the send ───────────

/// POST with an explicit `X-Forwarded-For`. The invite window (like signup's and
/// the device flow's) is a **process-wide** per-source bucket shared by every hub
/// in this binary, so each bounds test must claim its own source or it would
/// spend — and be spent by — its neighbours' budget.
async fn post_json_from(
    client: &reqwest::Client,
    base: &str,
    cookie: &str,
    path: &str,
    body: serde_json::Value,
    source: &str,
) -> reqwest::Response {
    client
        .post(format!("http://{base}{path}"))
        .header(reqwest::header::COOKIE, cookie)
        .header("x-forwarded-for", source)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// C1: a loop of org-invite POSTs from one account hits the org-scoped live-invite
/// cap and answers **429, not 400** — i.e. `store_err` grew its `CAP:` arm, and the
/// prefix never reaches the client.
#[tokio::test]
async fn org_invite_loop_is_capped_with_429() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.1.1";
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "cap-team" }), src).await.status().is_success());

    let mut capped: Option<reqwest::Response> = None;
    for i in 0..40 {
        let r = post_json_from(&client, &base, &alice, "/api/orgs/invites",
            serde_json::json!({ "email": format!("cap{i}@x.com") }), src).await;
        if r.status() != reqwest::StatusCode::OK {
            capped = Some(r);
            break;
        }
    }
    let r = capped.expect("an unbounded loop of invite POSTs is the whole bug");
    assert_eq!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS, "a cap is 429, never 400");
    let body = r.text().await.unwrap();
    assert!(!body.starts_with("CAP:"), "the store's prefix must never reach the client: {body}");
    assert!(
        body.contains("this team has too many pending invitations"),
        "the LIVE-INVITE cap is what fired here, not the looser per-source window: {body}"
    );
}

/// C1: the same for `POST /api/shares` — and it fires identically in both arms,
/// so the bound is never an account-existence oracle. `bob@x.com` HAS an account;
/// `stranger@x.com` does not; at the cap both answer 429 with the same body.
#[tokio::test]
async fn share_invite_loop_is_capped_identically_in_both_arms() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.2.1";

    let mut capped = None;
    for i in 0..40 {
        let r = post_json_from(&client, &base, &alice, "/api/shares",
            serde_json::json!({ "grantee_email": format!("scap{i}@x.com"), "machine": "laptop" }), src).await;
        if r.status() != reqwest::StatusCode::OK {
            capped = Some(r);
            break;
        }
    }
    let r = capped.expect("the share endpoint must be bounded too");
    assert_eq!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let unknown_body = r.text().await.unwrap();
    assert!(!unknown_body.starts_with("CAP:"), "prefix leaked: {unknown_body}");
    assert!(
        unknown_body.contains("too many pending invitations"),
        "the live-invite cap fired, not the looser per-source window: {unknown_body}"
    );

    // The known-account arm — the one the 0056 cap skipped entirely (it passes
    // `converted = true`) — answers with the same status and the same body.
    let known = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "bob@x.com", "machine": "laptop" }), src).await;
    assert_eq!(known.status(), reqwest::StatusCode::TOO_MANY_REQUESTS, "no arm is exempt from the cap");
    assert_eq!(known.text().await.unwrap(), unknown_body, "no oracle: same body in both arms");
}

/// C1: a loop of **Resends** on one invite is bounded too (a re-offer used to
/// return before the cap block was even reached), and — the explicit acceptance
/// criterion — reading the invite outbox is never throttled, because the window
/// lives inside the POST handler and not on the combined route.
#[tokio::test]
async fn resend_loop_is_bounded_but_the_outbox_never_is() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.3.1";
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "resend-team" }), src).await.status().is_success());

    // Every POST here is a RE-OFFER of the same invitation (same org, same
    // address): it never grows the live set, so only the per-source window bounds
    // it. Without that window this loop would run forever.
    let mut refused = None;
    for _ in 0..60 {
        let r = post_json_from(&client, &base, &alice, "/api/orgs/invites",
            serde_json::json!({ "email": "resend@x.com" }), src).await;
        if r.status() != reqwest::StatusCode::OK {
            refused = Some(r);
            break;
        }
    }
    let r = refused.expect("an unbounded Resend loop is a mail amplifier");
    assert_eq!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let body = r.text().await.unwrap();
    assert!(body.contains("slow down"), "the per-source window is what bounds a re-offer loop: {body}");

    // …and the GET half of the same combined route is untouched by it.
    let outbox = client
        .get(format!("http://{base}/api/orgs/invites"))
        .header(reqwest::header::COOKIE, &alice)
        .header("x-forwarded-for", src)
        .send()
        .await
        .unwrap();
    assert!(outbox.status().is_success(), "reading the invite outbox is never throttled");
    let rows: serde_json::Value = outbox.json().await.unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "the re-offers all landed on one row");
}

/// C3: header/body injection is refused at the boundary — an org name carrying
/// `\r\nBcc:` is refused at create, an address carrying it answers 400 **before**
/// any row is written, and `bob@intranet` (a self-hosted hub on a LAN) still works.
#[tokio::test]
async fn injection_shaped_names_and_addresses_are_refused() {
    let (base, _store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.4.1";

    // The org name — interpolated into the invitation body.
    let evil_org = post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "acme\r\nBcc: attacker@x" }), src).await;
    assert_eq!(evil_org.status(), reqwest::StatusCode::BAD_REQUEST, "a control char in a team name");
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "acme" }), src).await.status().is_success());

    // The address, on BOTH invite endpoints.
    let evil = "victim@x.com\r\nBcc: attacker@x.com";
    let org_bad = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": evil }), src).await;
    assert_eq!(org_bad.status(), reqwest::StatusCode::BAD_REQUEST);
    let share_bad = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": evil, "machine": "laptop" }), src).await;
    assert_eq!(share_bad.status(), reqwest::StatusCode::BAD_REQUEST);

    // …and nothing was persisted by either refusal.
    let outbox = get_json(&client, &base, &alice, "/api/orgs/invites").await;
    assert!(outbox.as_array().unwrap().is_empty(), "400 before the invite row is written: {outbox}");
    let shares = get_json(&client, &base, &alice, "/api/shares/outbox").await;
    assert!(shares.as_array().unwrap().is_empty(), "400 before the invite row is written: {shares}");

    // Deliverability is not the validator's job: a dotless corporate domain is
    // a legitimate invitee on a self-hosted hub and must still be accepted.
    let intranet = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "bob@intranet" }), src).await;
    assert!(intranet.status().is_success(), "bob@intranet: {}", intranet.status());
}

/// C1: the send ceilings. They bound **mail**, not rows — an invitation over one
/// is still created and its copyable link still returned; only the send is
/// skipped, and `delivery` stays NULL, which is exactly what the 0013 migration
/// defines NULL to mean ("no send was attempted"). Because the ceiling is checked
/// in the handler *before* the spawn, a capped invite spawns no task at all.
///
/// The pressure here is seeded onto a **different org**, so the only bound that
/// can fire is the hub-wide one — the ceiling that maps to reality, since the
/// relay's quota is per account and N orgs each under a per-org ceiling can still
/// exhaust it.
#[tokio::test]
async fn a_hub_wide_send_ceiling_skips_the_send_but_keeps_the_invitation() {
    let (base, store) = start_hub_with(&["claude-a"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.6.1";
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "sender-team" }), src).await.status().is_success());

    // Control: with the harness's capture transport and no pressure, an invite IS
    // mailed — so a NULL `delivery` below means something.
    let first = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "first@x.com" }), src).await;
    assert!(first.status().is_success());
    let first_id = first.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    let delivery_of = |id: String| {
        let (client, base, alice) = (client.clone(), base.clone(), alice.clone());
        async move {
            let outbox = get_json(&client, &base, &alice, "/api/orgs/invites").await;
            outbox.as_array().unwrap().iter()
                .find(|r| r["id"] == serde_json::json!(id))
                .and_then(|r| r["delivery"].as_str().map(str::to_string))
        }
    };
    let mut settled = None;
    for _ in 0..60 {
        if let Some(d) = delivery_of(first_id.clone()).await {
            settled = Some(d);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(settled.is_some(), "the control invite was mailed and stamped a receipt");

    // Seed a *different* org past the hub-wide ceiling. `status='revoked'` keeps
    // these rows out of the live-invite cap, so the only thing they move is the
    // 24h mail count.
    let bob_id = store.user_id_by_email("bob@x.com").await.unwrap();
    let noisy = store.org_create(&bob_id, "noisy-team").await.unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    for i in 0..cc_screen_hub::db::HUB_MAIL_PER_DAY {
        sqlx::query(
            "INSERT INTO org_invites
               (id, token, org_id, inviter_user_id, email, role, status, created_at, expires_at, emailed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'member', 'revoked', ?6, ?7, ?6)",
        )
        .bind(format!("seed-{i}"))
        .bind(format!("seed-tok-{i}"))
        .bind(&noisy)
        .bind(&bob_id)
        .bind(format!("seed{i}@x.com"))
        .bind(now)
        .bind(now + 86_400)
        .execute(store.pool())
        .await
        .unwrap();
    }

    // Alice's own org is nowhere near its per-org ceiling, but the hub is over its
    // account-wide one: the invitation is created, the link is returned, no send.
    let second = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "second@x.com" }), src).await;
    assert_eq!(second.status(), reqwest::StatusCode::OK, "a send ceiling never fails the invitation");
    let second: serde_json::Value = second.json().await.unwrap();
    assert!(second["invite_url"].as_str().is_some_and(|u| u.contains("/org-invite/")),
        "the copyable link — the channel that always worked — is still there: {second}");
    let second_id = second["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(delivery_of(second_id).await, None, "over the ceiling ⇒ no task was ever spawned");
}

// ── Proposal 0073 Part E1 — the send, asserted on real captured messages ──────

/// Everything the maildir holds right now, oldest first: the capture filename is
/// a zero-padded nanosecond stamp plus a monotonic sequence, so a lexical sort is
/// chronological. A missing directory is an empty maildir, not an error — a hub
/// with no transport never creates one.
fn maildir_messages(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect()
}

/// Wait for the maildir to hold `want` messages, then let it settle and hand back
/// everything it holds.
///
/// The send is `tokio::spawn`ed and never awaited ("the delivery decision" §2 — a
/// 20s `SEND_TIMEOUT` has no business on a request path), so the capture file
/// appears shortly *after* the HTTP response returns; reading the directory
/// straight after the POST is exactly the flake this helper exists to prevent.
/// It is a bounded poll rather than a fixed sleep in both directions: it returns
/// as soon as the mail lands, and the short settle afterwards is what lets a
/// caller assert `== want` and mean it — a second, unwanted message shows up as
/// `want + 1` instead of being raced past.
async fn mail_captured(dir: &Path, want: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline && maildir_messages(dir).len() < want {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    maildir_messages(dir)
}

/// The header block of a captured message, lowercased and newline-normalized.
fn mail_headers(raw: &str) -> String {
    raw.split("\r\n\r\n").next().unwrap_or_default().replace("\r\n", "\n").to_lowercase()
}

/// The body of a captured message, with quoted-printable soft line breaks joined
/// back up so a wrapped URL — or a token that straddles a fold — compares as one
/// string.
fn mail_body(raw: &str) -> String {
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(raw);
    body.replace("=\r\n", "").replace("=\n", "")
}

/// Every recipient header line (`to:`/`cc:`/`bcc:`) in a captured message. Only
/// lines that start at column 0 count — a folded continuation is indented, and is
/// part of the header above it, not a new one.
fn recipient_headers(raw: &str) -> Vec<String> {
    mail_headers(raw)
        .lines()
        .filter(|l| l.starts_with("to:") || l.starts_with("cc:") || l.starts_with("bcc:"))
        .map(str::to_string)
        .collect()
}

/// The **logical** header names of a captured message. A folded continuation line
/// (RFC 5322 marks one with leading whitespace) belongs to the header above it and
/// is deliberately not counted as a new header — that distinction is the entire
/// subject of the injection test.
fn header_names(raw: &str) -> Vec<String> {
    mail_headers(raw)
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('\t'))
        .filter_map(|l| l.split_once(':').map(|(name, _)| name.trim().to_string()))
        .collect()
}

/// The invite token as the **recipient** reads it: out of the message body, not
/// out of the create response. `path` is `/invite/` or `/org-invite/`.
fn token_in(body: &str, path: &str) -> String {
    let at = body.find(path).unwrap_or_else(|| panic!("no {path} link in the message:\n{body}"));
    body[at + path.len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// E1 (1): the mailer is not the account-existence oracle [0042] and [0056] C2
/// exist to prevent. Inviting an address that HAS an account and one that does
/// not produces **one message each**, and the two are the same message once the
/// address and the token are substituted out — the Rust twin of
/// `ShareForm.test.tsx`'s normalize-and-compare on the rendered copy.
#[tokio::test]
async fn invite_mails_both_arms_identically() {
    let (base, _store, maildir) = start_hub_capturing().await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.7.1";

    // This test owns both addresses. `bob@x.com` is shared with half the file and
    // the destination frequency cap (C1) is a process-global bucket, so borrowing
    // him would make the known arm's message count depend on test ordering.
    let known = "known-e1@x.com";
    let ghost = "ghost-e1@x.com";
    let _ = signup_ok(&client, &base, known, "known-e1-password", "10.73.7.2").await;

    let a = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": known, "machine": "laptop" }), src).await;
    let b = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": ghost, "machine": "laptop" }), src).await;
    assert_eq!(a.status(), reqwest::StatusCode::OK, "known arm");
    assert_eq!(b.status(), reqwest::StatusCode::OK, "unknown arm");
    let (aj, bj) = (a.json::<serde_json::Value>().await.unwrap(), b.json::<serde_json::Value>().await.unwrap());
    let tok_of = |v: &serde_json::Value| v["invite_url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let (a_tok, b_tok) = (tok_of(&aj), tok_of(&bj));

    let msgs = mail_captured(&maildir, 2).await;
    assert_eq!(msgs.len(), 2, "exactly one message per invitation, in both arms");
    let addressed_to = |email: &str| {
        msgs.iter()
            .find(|m| mail_headers(m).contains(&format!("to: {email}")))
            .unwrap_or_else(|| panic!("no message addressed to {email}"))
            .clone()
    };
    let (a_msg, b_msg) = (addressed_to(known), addressed_to(ghost));

    // The subject does not branch on the outcome at all — it carries neither the
    // recipient nor the token, so it must be byte-identical.
    let subject = |raw: &str| {
        mail_headers(raw).lines().find(|l| l.starts_with("subject:")).unwrap_or_default().to_string()
    };
    assert_eq!(subject(&a_msg), subject(&b_msg), "the subject is the same in both arms");

    // …and so is the body, once the two things that legitimately differ are
    // normalized away. Anything else — a "they already have an account" line, a
    // different call to action — would be the oracle, readable by anyone who can
    // type two addresses into the invite form.
    let normalize = |raw: &str, email: &str, tok: &str| mail_body(raw).replace(email, "EMAIL").replace(tok, "TOKEN");
    let (a_norm, b_norm) = (normalize(&a_msg, known, &a_tok), normalize(&b_msg, ghost, &b_tok));
    assert!(a_norm.contains("TOKEN"), "the token really was substituted: {a_norm}");
    assert_eq!(a_norm, b_norm, "the two invitation bodies differ only by address + token");
}

/// E1 (2): [0056] C4 as a test — *"the link is not an access capability"*. Mail
/// makes a forwarded or archived token far more likely, so this property is what
/// makes mailing one acceptable at all: a **different** authenticated account
/// holding the mailed token gets nothing, on either invite surface.
#[tokio::test]
async fn mailed_token_is_not_a_capability() {
    let (base, _store, maildir) = start_hub_capturing().await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234-long").await;
    let src = "10.73.8.1";

    // ── the share link ────────────────────────────────────────────────────────
    let created = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost-e2@x.com", "machine": "laptop" }), src).await;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let msgs = mail_captured(&maildir, 1).await;
    assert_eq!(msgs.len(), 1, "one invitation, one message");
    let token = token_in(&mail_body(&msgs[0]), "/invite/");
    assert_eq!(
        token,
        created.json::<serde_json::Value>().await.unwrap()["invite_url"].as_str().unwrap().rsplit('/').next().unwrap(),
        "the mailed link is the invitation's own link"
    );

    // Bob reads the link while signed in as himself. The read resolves — the
    // token identifies the invitation, that is its whole job…
    let info = client
        .get(format!("http://{base}/api/invite/{token}"))
        .header(reqwest::header::COOKIE, &bob)
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), reqwest::StatusCode::OK);
    assert_eq!(info.json::<serde_json::Value>().await.unwrap()["email"], "ghost-e2@x.com");

    // …and grants him nothing: no invite attached, no session, no attach.
    assert!(get_json(&client, &base, &bob, "/api/shares/inbox").await.as_array().unwrap().is_empty(),
        "a forwarded token attaches nothing to the wrong account");
    assert_eq!(session_names(&client, &base, &bob).await, vec!["claude-b"], "no access to alice's machine");
    assert!(!can_attach(&base, &bob, "claude-a").await);

    // The refusal is about identity, not a burnt token: the invited address still
    // converts the very same token the normal way.
    let ghost = signup_ok(&client, &base, "ghost-e2@x.com", "ghost-e2-password", "10.73.8.2").await;
    assert_eq!(get_json(&client, &base, &ghost, "/api/shares/inbox").await.as_array().unwrap().len(), 1,
        "the addressee's own signup converts it");

    // ── the team link ─────────────────────────────────────────────────────────
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "capability-team" }), src).await.status().is_success());
    let inv = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "ghost-e2-team@x.com" }), src).await;
    assert_eq!(inv.status(), reqwest::StatusCode::OK);
    let inv_id = inv.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();
    let msgs = mail_captured(&maildir, 2).await;
    assert_eq!(msgs.len(), 2, "one message per invitation here too");
    let org_token = token_in(&mail_body(&msgs[1]), "/org-invite/");

    // Bob resolves the team link…
    let info = client
        .get(format!("http://{base}/api/org-invite/{org_token}"))
        .header(reqwest::header::COOKIE, &bob)
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), reqwest::StatusCode::OK);
    assert_eq!(info.json::<serde_json::Value>().await.unwrap()["email"], "ghost-e2-team@x.com");

    // …and is still in no team: the link put nothing in his inbox, and accepting
    // the invitation it names is refused because he is not the addressee.
    assert!(get_json(&client, &base, &bob, "/api/orgs/invites/inbox").await.as_array().unwrap().is_empty(),
        "the mailed team link attaches nothing to the wrong account");
    assert_eq!(
        post_json(&client, &base, &bob, &format!("/api/orgs/invites/{inv_id}/accept"), serde_json::json!({})).await.status(),
        reqwest::StatusCode::NOT_FOUND,
        "holding the token is not being the addressee"
    );
    assert_eq!(get_resp(&client, &base, &bob, "/api/orgs/mine").await.status(), reqwest::StatusCode::NOT_FOUND,
        "still in no team");
}

/// E1 (3): the self-hoster's hub. With no transport configured the mailer is
/// inert — nothing captured, no task spawned, `delivery` left NULL (the 0013
/// migration's "no send was attempted") — and the create response is byte-shape
/// identical, still carrying the copyable link that has always been the channel.
#[tokio::test]
async fn disabled_mailer_writes_nothing_and_still_returns_a_link() {
    let (base, store, maildir) = start_hub_no_mail().await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.9.1";
    assert!(post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": "self-hosted-team" }), src).await.status().is_success());

    // The team invite: 200 with exactly {id, status, invite_url}.
    let inv = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "self-host-e3@x.com" }), src).await;
    assert_eq!(inv.status(), reqwest::StatusCode::OK, "no mailer is not an error");
    let inv: serde_json::Value = inv.json().await.unwrap();
    let obj = inv.as_object().unwrap();
    assert_eq!(obj.len(), 3, "the response shape does not move on a mail-less hub: {inv}");
    for k in ["id", "status", "invite_url"] {
        assert!(obj.contains_key(k), "missing {k} in {inv}");
    }
    let org_token = inv["invite_url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    assert!(inv["invite_url"].as_str().unwrap().contains("/org-invite/") && !org_token.is_empty());

    // The share invite, same story.
    let share = post_json_from(&client, &base, &alice, "/api/shares",
        serde_json::json!({ "grantee_email": "ghost-e3@x.com", "machine": "laptop" }), src).await;
    assert_eq!(share.status(), reqwest::StatusCode::OK);
    let share: serde_json::Value = share.json().await.unwrap();
    let share_token = share["invite_url"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    assert!(share["invite_url"].as_str().unwrap().contains("/invite/") && !share_token.is_empty());

    // Both links are usable — this is the channel, not a decoration.
    assert!(client.get(format!("http://{base}/api/org-invite/{org_token}")).send().await.unwrap().status().is_success());
    assert!(client.get(format!("http://{base}/api/invite/{share_token}")).send().await.unwrap().status().is_success());

    // Nothing was written and nothing was spawned. The assertion is a negative, so
    // the wait is a floor on how long a spawned send would have had to appear —
    // generous next to the ~ms a capture write takes.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(maildir_messages(&maildir).is_empty(), "a hub with no transport captures nothing");
    assert!(!maildir.exists(), "…and never even creates the directory");

    // `delivery` is NULL on both rows: no send was attempted, which is exactly
    // what the column means and what the outbox shows the inviter.
    let outbox = get_json(&client, &base, &alice, "/api/orgs/invites").await;
    let row = outbox.as_array().unwrap().iter().find(|r| r["id"] == inv["id"]).expect("invite in the outbox");
    assert!(row["delivery"].is_null(), "no transport ⇒ delivery stays NULL: {row}");
    let (delivery, emailed_at) = sqlx::query_as::<_, (Option<String>, Option<i64>)>(
        "SELECT delivery, emailed_at FROM email_invites",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert!(delivery.is_none() && emailed_at.is_none(), "the share row was never stamped either");
}

/// E1 (4): `/api/me` reports what this hub can actually do — the twin of the
/// `billing:false` assertion. The frontend branches its invite copy on it (D2),
/// so a hub that cannot mail must never claim it can.
#[tokio::test]
async fn me_reports_mail_false_without_a_transport() {
    let client = reqwest::Client::new();

    let (base, _store, _maildir) = start_hub_no_mail().await;
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let me = get_json(&client, &base, &alice, "/api/me").await;
    assert_eq!(me["mail"], serde_json::Value::Bool(false), "no transport → mail:false");

    let (base, _store, _maildir) = start_hub_capturing().await;
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let me = get_json(&client, &base, &alice, "/api/me").await;
    assert_eq!(me["mail"], serde_json::Value::Bool(true), "a configured transport → mail:true");
}

/// E1 (5): an org name carrying `\r\nBcc:` can never add a recipient.
///
/// Two halves, because C3 closed the front door: the HTTP path is **refused at
/// create**, so the name can no longer reach the mailer that way at all — which
/// is the stronger outcome, and is asserted here rather than assumed. To keep the
/// structural defense covered anyway (a future rename endpoint, a hand-edited
/// database, a migration from an older hub that had no validator), the second
/// half seeds the poisoned name straight into the tables with SQL — `org_create`
/// validates too, so raw SQL is the only way past both — and drives a real invite
/// through the real handler and the real mailer.
#[tokio::test]
async fn an_injected_org_name_cannot_add_a_recipient() {
    let (base, store, maildir) = start_hub_capturing().await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1-long").await;
    let src = "10.73.10.1";
    let evil = "Acme\r\nBcc: x@y";

    // Half one: the name never gets in through the front door (C3).
    let refused = post_json_from(&client, &base, &alice, "/api/orgs",
        serde_json::json!({ "name": evil }), src).await;
    assert_eq!(refused.status(), reqwest::StatusCode::BAD_REQUEST, "a control char in a team name is refused");

    // Half two: seed it anyway, past both validators, and mail an invitation.
    let alice_id = store.user_id_by_email("alice@x.com").await.unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    sqlx::query("INSERT INTO orgs (id, name, plan, created_at) VALUES ('evil-org', ?1, 'team', ?2)")
        .bind(evil)
        .bind(now)
        .execute(store.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO org_members (org_id, user_id, role, created_at) VALUES ('evil-org', ?1, 'owner', ?2)")
        .bind(&alice_id)
        .bind(now)
        .execute(store.pool())
        .await
        .unwrap();

    let inv = post_json_from(&client, &base, &alice, "/api/orgs/invites",
        serde_json::json!({ "email": "victim-e5@x.com" }), src).await;
    assert_eq!(inv.status(), reqwest::StatusCode::OK);

    let msgs = mail_captured(&maildir, 1).await;
    assert_eq!(msgs.len(), 1, "one invitation, one message — never a second copy");
    let headers = mail_headers(&msgs[0]);
    assert_eq!(
        recipient_headers(&msgs[0]),
        vec!["to: victim-e5@x.com".to_string()],
        "exactly one recipient — the CRLF never became a header separator:\n{}",
        msgs[0]
    );
    assert!(!header_names(&msgs[0]).iter().any(|n| n == "bcc" || n == "cc"), "no injected recipient header: {headers}");
    // What the encoder actually does is worth pinning, because "the Bcc bytes are
    // gone" would be the wrong assertion: lettre RFC 2047-encodes the word holding
    // the CRLF (`=?utf-8?b?…?=`) and the words after it survive as plain text on a
    // *folded continuation* — leading whitespace, so RFC 5322 reads them as more of
    // the Subject. Inert either way; the thing that must never happen is a line
    // starting a header of the attacker's choosing.
    for line in headers.lines().filter(|l| l.contains("x@y")) {
        assert!(
            line.starts_with(' ') || line.starts_with('\t'),
            "the injected fragment reached the start of a header line: {line}"
        );
    }
    // The bytes are still *in* the message — as inert body/subject text, which is
    // the whole point: lettre's typed builder encodes them instead of splicing.
    assert!(mail_body(&msgs[0]).contains("Bcc: x@y"), "the name is delivered as text, not as a header");
}
