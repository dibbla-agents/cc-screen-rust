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
    // Register both agents online with a dummy uplink channel + a distinct session.
    let (txa, _rxa) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&alice_agent, &alice, "laptop", "a.local", vec![], txa)
        .set_sessions(vec![sess("claude-a")]);
    let (txb, _rxb) = mpsc::channel::<Vec<u8>>(8);
    registry.register_agent(&bob_agent, &bob, "laptop", "b.local", vec![], txb)
        .set_sessions(vec![sess("claude-b")]);

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
