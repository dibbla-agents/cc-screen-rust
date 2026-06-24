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
    start_hub_with(&["claude-a"], &["claude-b"]).await
}

/// As above, but with explicit session lists per tenant (so a sharing test can
/// give Alice a second session that a *session* share must NOT expose).
async fn start_hub_with(alice_sessions: &[&str], bob_sessions: &[&str]) -> String {
    let tmp = std::env::temp_dir().join(format!("ccr-hub-mt-{}-{}", std::process::id(), now_nanos()));
    let _ = std::fs::create_dir_all(&tmp);
    let store = SqliteStore::connect(&format!("sqlite://{}/hub.db", tmp.display()))
        .await
        .expect("open store");
    // Two tenants, each with a "laptop" — the label collides across tenants.
    let alice = store.create_user("alice@x.com", "alicepass1").await.unwrap();
    let bob = store.create_user("bob@x.com", "bobpass1234").await.unwrap();
    let (_atok, alice_agent) = store.upsert_agent(&alice, "laptop").await.unwrap();
    let (_btok, bob_agent) = store.upsert_agent(&bob, "laptop").await.unwrap();

    let registry = Registry::new();
    // Register both agents online with a dummy uplink channel + their sessions.
    let (txa, _rxa) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&alice_agent, &alice, "laptop", "a.local", vec![], txa)
        .set_sessions(alice_sessions.iter().map(|n| sess(n)).collect());
    let (txb, _rxb) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&bob_agent, &bob, "laptop", "b.local", vec![], txb)
        .set_sessions(bob_sessions.iter().map(|n| sess(n)).collect());
    // Keep the fake agents' uplink receivers alive for the life of the hub so a
    // relayed Attach frame doesn't fail the send (the WS still upgrades regardless).
    std::mem::forget((_rxa, _rxb));

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
        tenancy: Tenancy::Multi(Arc::new(store)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, cc_screen_hub::build_router(hub)).await.unwrap() });
    format!("{addr}")
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

    let alice = login(&client, &base, "alice@x.com", "alicepass1").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234").await;

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
    let base = start_hub_with(&["claude-a", "claude-a2"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234").await;

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
    let base = start_hub_with(&["claude-a", "claude-a2"], &["claude-b"]).await;
    let client = reqwest::Client::new();
    let alice = login(&client, &base, "alice@x.com", "alicepass1").await;
    let bob = login(&client, &base, "bob@x.com", "bobpass1234").await;

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
