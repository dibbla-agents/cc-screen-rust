//! Multi-tenant tenant-isolation tests (proposal 0001 Phase 1 "Done when").
//!
//! Two hand-created users, each owning an agent that happens to share the machine
//! label "laptop", talk to the **real** multi-tenant hub router. We assert the
//! §4.1 keystone end-to-end: neither user can list, control, or attach to the
//! other's agent, and an unauthenticated request is refused. Only compiled with
//! `--features multi-tenant`.
#![cfg(feature = "multi-tenant")]

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
async fn start_hub_with(alice_sessions: &[&str], bob_sessions: &[&str]) -> (String, Arc<SqliteStore>) {
    let tmp = std::env::temp_dir().join(format!("ccr-hub-mt-{}-{}", std::process::id(), now_nanos()));
    let _ = std::fs::create_dir_all(&tmp);
    let store = SqliteStore::connect(&format!("sqlite://{}/hub.db", tmp.display()))
        .await
        .expect("open store");
    // Two tenants, each with a "laptop" — the label collides across tenants.
    let alice = store.create_user("alice@x.com", "alicepass1-long").await.unwrap();
    let bob = store.create_user("bob@x.com", "bobpass1234-long").await.unwrap();
    let (_atok, alice_agent) = store.upsert_agent(&alice, "laptop").await.unwrap();
    let (_btok, bob_agent) = store.upsert_agent(&bob, "laptop").await.unwrap();

    let registry = Registry::new();
    // Register both agents online with a dummy uplink channel + their sessions.
    let (txa, _rxa) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&alice_agent, &alice, "laptop", "a.local", vec![], Vec::new(), txa)
        .set_sessions(alice_sessions.iter().map(|n| sess(n)).collect());
    let (txb, _rxb) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&bob_agent, &bob, "laptop", "b.local", vec![], Vec::new(), txb)
        .set_sessions(bob_sessions.iter().map(|n| sess(n)).collect());
    // Keep the fake agents' uplink receivers alive for the life of the hub so a
    // relayed Attach frame doesn't fail the send (the WS still upgrades regardless).
    std::mem::forget((_rxa, _rxb));

    let store = Arc::new(store);
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
        tenancy: Tenancy::Multi(store.clone()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, cc_screen_hub::build_router(hub)).await.unwrap() });
    (format!("{addr}"), store)
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
