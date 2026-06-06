//! End-to-end relay tests for the hub, in-process over loopback WebSockets.
//!
//! A *fake agent* (a `tokio-tungstenite` client speaking the agent↔hub envelope)
//! dials the real hub router; a *fake client* (the browser/`ccs` side) attaches
//! through the hub. We assert the terminal relay (snapshot-first + input echo),
//! the machine-tagged session list, and the auth boundary — all without a real
//! PTY agent. Everything binds `127.0.0.1:0` (ephemeral) and uses a temp config
//! dir, so it's hermetic and parallel-safe.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cc_screen_auth::Auth;
use cc_screen_hub::{build_router, registry::Registry, state::HubState};
use cc_screen_protocol::hub::{decode_frame, encode_frame, AgentMsg, HubMsg, HUB_PROTO_VERSION};
use cc_screen_protocol::{SessionInfo, ToolInfo};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Start the hub on an ephemeral port; return its `host:port`.
async fn start_hub(client_auth: Auth, agent_tokens: &[(&str, &str)]) -> String {
    let tokens: HashMap<String, String> =
        agent_tokens.iter().map(|(m, t)| (m.to_string(), t.to_string())).collect();
    let tmp = std::env::temp_dir().join(format!("ccr-hub-e2e-{}-{}", std::process::id(), agent_tokens.len()));
    let _ = std::fs::create_dir_all(&tmp);
    let hub = HubState {
        registry: Registry::new(),
        agent_tokens: Arc::new(tokens),
        client_auth,
        origin: cc_screen_auth::OriginPolicy::default(),
        login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
        config_dir: tmp.clone(),
        push: Arc::new(cc_screen_push::Push::new(&tmp)),
        bulk: Default::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = build_router(hub);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("{addr}")
}

fn sess(name: &str) -> SessionInfo {
    SessionInfo {
        name: name.into(),
        tool: "shell".into(),
        short: name.into(),
        attached: false,
        activity: 0,
        last_input_at: 0,
        busy_since: 0,
        preview: String::new(),
        waiting: false,
        cwd: String::new(),
        machine: String::new(),
    }
}

async fn connect(url: &str, token: Option<&str>) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = url.into_client_request().unwrap();
    if let Some(t) = token {
        req.headers_mut()
            .insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

async fn send(ws: &mut Ws, msg: &AgentMsg, payload: &[u8]) {
    ws.send(Message::Binary(encode_frame(msg, payload))).await.unwrap();
}

/// Spawn a fake agent that registers `machine_id`, advertises one session, and
/// answers attaches with an RIS snapshot then echoes input as output.
async fn spawn_fake_agent(hub: &str, machine_id: &str, token: Option<&str>, session: &str) {
    let url = format!("ws://{hub}/agent/ws");
    let mut ws = connect(&url, token).await.expect("agent connects");
    send(
        &mut ws,
        &AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: machine_id.into(),
            hostname: machine_id.into(),
            agent_version: "test".into(),
            tools: vec![],
        },
        b"",
    )
    .await;
    send(&mut ws, &AgentMsg::Sessions { sessions: vec![sess(session)] }, b"").await;

    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            let Message::Binary(buf) = msg else { continue };
            let Ok((hub_msg, payload)) = decode_frame::<HubMsg>(&buf) else { continue };
            match hub_msg {
                HubMsg::Attach { ch, .. } => {
                    // Snapshot first (RIS-prefixed), exactly like a real agent.
                    send(&mut ws, &AgentMsg::Snapshot { ch }, b"\x1bcHELLO_FROM_AGENT").await;
                }
                HubMsg::Input { ch } => {
                    // Echo the client's input back as output.
                    let bytes = payload.to_vec();
                    send(&mut ws, &AgentMsg::Output { ch }, &bytes).await;
                }
                HubMsg::Ping => send(&mut ws, &AgentMsg::Pong, b"").await,
                _ => {}
            }
        }
    });
}

/// Read binary WS frames until one satisfies `pred` (or time out).
async fn read_until<F: Fn(&[u8]) -> bool>(ws: &mut Ws, pred: F) -> bool {
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                if pred(&b) {
                    return true;
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => return false,
        }
    }
    false
}

#[tokio::test]
async fn terminal_relay_snapshot_and_input_through_hub() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    spawn_fake_agent(&hub, "boxA", None, "shell-x").await;

    // The session shows up in the union, tagged with its machine.
    let mut listed = false;
    for _ in 0..50 {
        let body: Vec<SessionInfo> =
            reqwest::get(format!("http://{hub}/api/sessions")).await.unwrap().json().await.unwrap();
        if body.iter().any(|s| s.name == "shell-x" && s.machine == "boxA") {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(listed, "session appears at the hub tagged machine=boxA");

    // Attach through the hub; first frame is the RIS-prefixed snapshot.
    let mut client = connect(&format!("ws://{hub}/api/ws?machine=boxA&session=shell-x"), None)
        .await
        .expect("client attaches");
    assert!(
        read_until(&mut client, |b| b.starts_with(b"\x1bc") && b.windows(6).any(|w| w == b"HELLO_")).await,
        "client receives the RIS snapshot through the hub"
    );

    // Typed input round-trips client → hub → agent → hub → client.
    client.send(Message::Binary(b"PING_INPUT_42".to_vec())).await.unwrap();
    assert!(
        read_until(&mut client, |b| b.windows(13).any(|w| w == b"PING_INPUT_42")).await,
        "input echoes back through the hub"
    );
}

#[tokio::test]
async fn client_without_credential_gets_401() {
    // Auth enabled (password set) → an unauthenticated request is rejected.
    let auth = Auth::new(Some("pw".into()), Some("tok".into()), [7u8; 32]);
    let hub = start_hub(auth, &[]).await;
    let resp = reqwest::get(format!("http://{hub}/api/sessions")).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The same request WITH the bearer token is allowed.
    let client = reqwest::Client::new();
    let ok = client
        .get(format!("http://{hub}/api/sessions"))
        .bearer_auth("tok")
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn agent_with_wrong_uplink_token_is_rejected() {
    // Hub configured to require boxA's token; an agent presenting the wrong one
    // must not end up registered/listed.
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[("boxA", "right-token")]).await;

    // Wrong token: the handshake is refused (no agent token matches), so connect
    // fails or the session never lists.
    let _ = connect(&format!("ws://{hub}/agent/ws"), Some("wrong-token")).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let body: Vec<SessionInfo> = reqwest::Client::new()
        .get(format!("http://{hub}/api/sessions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.is_empty(), "an agent with a bad uplink token must not register");

    // The correct token registers fine.
    spawn_fake_agent(&hub, "boxA", Some("right-token"), "shell-ok").await;
    let mut listed = false;
    for _ in 0..50 {
        let body: Vec<SessionInfo> = reqwest::Client::new()
            .get(format!("http://{hub}/api/sessions"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body.iter().any(|s| s.machine == "boxA") {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(listed, "the correctly-tokened agent registers");
}

/// Like `spawn_fake_agent` but the attach snapshot carries the machine id, so a
/// test can tell *which* agent served an attach — needed to prove routing when
/// two machines own a session of the same name.
async fn spawn_fake_agent_marked(hub: &str, machine_id: &str, session: &str) {
    let url = format!("ws://{hub}/agent/ws");
    let mut ws = connect(&url, None).await.expect("agent connects");
    let mid = machine_id.to_string();
    send(
        &mut ws,
        &AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: mid.clone(),
            hostname: mid.clone(),
            agent_version: "test".into(),
            tools: vec![],
        },
        b"",
    )
    .await;
    send(&mut ws, &AgentMsg::Sessions { sessions: vec![sess(session)] }, b"").await;
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws.next().await {
            let Message::Binary(buf) = msg else { continue };
            let Ok((hub_msg, _)) = decode_frame::<HubMsg>(&buf) else { continue };
            match hub_msg {
                HubMsg::Attach { ch, .. } => {
                    let snap = format!("\x1bcSNAP_{mid}");
                    send(&mut ws, &AgentMsg::Snapshot { ch }, snap.as_bytes()).await;
                }
                HubMsg::Ping => send(&mut ws, &AgentMsg::Pong, b"").await,
                _ => {}
            }
        }
    });
}

/// Two machines, each owning a session of the SAME name. This is the exact case
/// the PWA's machine-threading closes: a client that sends an explicit
/// `?machine=` reaches the right agent, whereas a name-only (machine-less)
/// attach is ambiguous and gets no relay (`registry::resolve` → None).
#[tokio::test]
async fn explicit_machine_disambiguates_same_named_session() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    spawn_fake_agent_marked(&hub, "boxA", "shell-x").await;
    spawn_fake_agent_marked(&hub, "boxB", "shell-x").await;

    // Both same-named sessions list, distinguished only by `machine`.
    let mut both = false;
    for _ in 0..50 {
        let body: Vec<SessionInfo> =
            reqwest::get(format!("http://{hub}/api/sessions")).await.unwrap().json().await.unwrap();
        let a = body.iter().any(|s| s.name == "shell-x" && s.machine == "boxA");
        let b = body.iter().any(|s| s.name == "shell-x" && s.machine == "boxB");
        if a && b {
            both = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(both, "both machines' same-named sessions are listed");

    // Explicit machine routes to the matching agent.
    let mut ca = connect(&format!("ws://{hub}/api/ws?machine=boxA&session=shell-x"), None)
        .await
        .expect("attach A");
    assert!(
        read_until(&mut ca, |b| b.starts_with(b"\x1bc") && b.windows(9).any(|w| w == b"SNAP_boxA")).await,
        "machine=boxA reaches boxA"
    );
    let mut cb = connect(&format!("ws://{hub}/api/ws?machine=boxB&session=shell-x"), None)
        .await
        .expect("attach B");
    assert!(
        read_until(&mut cb, |b| b.starts_with(b"\x1bc") && b.windows(9).any(|w| w == b"SNAP_boxB")).await,
        "machine=boxB reaches boxB"
    );

    // Name-only attach is ambiguous → the hub refuses or relays nothing. (This
    // is precisely why the PWA now threads `machine`.)
    let amb = connect(&format!("ws://{hub}/api/ws?session=shell-x"), None).await;
    let blocked = match amb {
        Err(_) => true,
        Ok(mut c) => !read_until(&mut c, |b| b.starts_with(b"\x1bc")).await,
    };
    assert!(blocked, "a machine-less attach to a colliding name gets no relay");
}

/// Fix 4: in open-uplink mode, a second peer registering as an already-online
/// machine must be rejected (closed), not silently displace the live agent.
#[tokio::test]
async fn open_mode_rejects_duplicate_online_machine() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    spawn_fake_agent(&hub, "boxA", None, "shell-x").await;

    // Wait until boxA is listed (online).
    let mut listed = false;
    for _ in 0..50 {
        let body: Vec<SessionInfo> =
            reqwest::get(format!("http://{hub}/api/sessions")).await.unwrap().json().await.unwrap();
        if body.iter().any(|s| s.machine == "boxA") {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(listed, "boxA online");

    // An impostor dials in as boxA. The hub must close it.
    let mut ws2 = connect(&format!("ws://{hub}/agent/ws"), None).await.expect("connects");
    send(
        &mut ws2,
        &AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: "boxA".into(),
            hostname: "boxA".into(),
            agent_version: "evil".into(),
            tools: vec![],
        },
        b"",
    )
    .await;
    let mut closed = false;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_secs(2), ws2.next()).await {
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => break,
        }
    }
    assert!(closed, "the impostor's duplicate registration is rejected");

    // The original boxA is still online and serving.
    let body: Vec<SessionInfo> =
        reqwest::get(format!("http://{hub}/api/sessions")).await.unwrap().json().await.unwrap();
    assert!(body.iter().any(|s| s.machine == "boxA"), "original boxA stays connected");
}

/// Fix 3: a bulk dial-back with an unknown / unmatched nonce is refused (no slot
/// to claim), so a peer can't fabricate an id to capture a transfer.
#[tokio::test]
async fn bulk_dialback_with_unknown_nonce_is_rejected() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    // No transfer is in flight, so any nonce is unknown → the upgrade is refused.
    let res = connect(
        &format!("ws://{hub}/agent/bulk?id=made-up-nonce&machine=boxA"),
        None,
    )
    .await;
    assert!(res.is_err(), "an unknown bulk nonce must not upgrade");
}

/// New Session needs the chosen agent's tool list. Agents register their tools at
/// uplink time; the hub serves them from the registry (no agent round-trip).
/// Without this route New Session's Create button stays disabled (no tool to
/// pick), which is exactly the bug this guards against.
#[tokio::test]
async fn hub_serves_resolved_agent_tools() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let url = format!("ws://{hub}/agent/ws");
    let mut ws = connect(&url, None).await.expect("agent connects");
    send(
        &mut ws,
        &AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: "boxA".into(),
            hostname: "boxA".into(),
            agent_version: "test".into(),
            tools: vec![ToolInfo { cmd: "cc".into(), prefix: "claude".into(), extra_dirs: None }],
        },
        b"",
    )
    .await;
    send(&mut ws, &AgentMsg::Sessions { sessions: vec![] }, b"").await;

    let mut got = false;
    for _ in 0..50 {
        let body: Vec<ToolInfo> = reqwest::get(format!("http://{hub}/api/tools?machine=boxA"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body.iter().any(|t| t.cmd == "cc" && t.prefix == "claude") {
            got = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(got, "hub /api/tools returns the resolved agent's registered tools");
}
