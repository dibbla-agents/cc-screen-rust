//! In-process hub + fake-agent test doubles (proposal 0059 B1).
//!
//! Hoisted out of `crates/hub/tests/e2e.rs` so **two** suites can share one
//! implementation of the agent↔hub wire contract: the hub's own e2e suite (the
//! first consumer, proving the hoist is behaviour-neutral) and the `ccs` TUI
//! e2e harness, which drives a real `App` against `start_hub()` + a fake agent.
//! Copying these into the TUI instead would fork the wire-contract double, and
//! drift between the two doubles is exactly the bug class the harness exists to
//! catch — so they live here, behind `feature = "test-support"`.
//!
//! Everything binds `127.0.0.1:0` (ephemeral) under a temp config dir, so it's
//! hermetic and parallel-safe.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cc_screen_auth::Auth;
use cc_screen_protocol::hub::{
    decode_frame, encode_frame, AgentMsg, Cmd, CmdResult, HubMsg, CAP_ASSISTANT_INSTALL,
    CAP_ASSISTANT_UPDATE, HUB_PROTO_VERSION,
};
use cc_screen_protocol::{RestorableSession, SessionInfo, ToolInfo, UpdateJob};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::registry::Registry;
use crate::state::HubState;

pub type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Start the hub on an ephemeral port; return its `host:port`.
///
/// Verbatim hoist of the former `e2e.rs::start_hub` — single-tenant, in-memory
/// registry, disabled summarizer, temp config dir.
pub async fn start_hub(client_auth: Auth, agent_tokens: &[(&str, &str)]) -> String {
    let tokens: HashMap<String, String> =
        agent_tokens.iter().map(|(m, t)| (m.to_string(), t.to_string())).collect();
    let tmp = std::env::temp_dir().join(format!(
        "ccr-hub-e2e-{}-{}-{}",
        std::process::id(),
        agent_tokens.len(),
        // A cheap per-call salt so parallel `start_hub`s never share a config dir
        // (they otherwise collide on pid+len). Address is unique post-bind, but
        // the dir is created before that, so derive it from a fresh atomic.
        next_salt(),
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let hub = HubState {
        registry: Registry::new(),
        agent_tokens: Arc::new(tokens),
        allow_open_uplink: false,
        client_auth,
        origin: cc_screen_auth::OriginPolicy::default(),
        login_throttle: Arc::new(cc_screen_auth::LoginThrottle::new()),
        config_dir: tmp.clone(),
        push: Arc::new(cc_screen_push::Push::new(&tmp)),
        bulk: Default::default(),
        summary: Arc::new(crate::summarizer::Summarizer::disabled()),
        tenancy: crate::state::Tenancy::Single,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = crate::build_router(hub);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("{addr}")
}

fn next_salt() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SALT: AtomicU64 = AtomicU64::new(0);
    SALT.fetch_add(1, Ordering::Relaxed)
}

/// A minimal `SessionInfo` for `name` (tool `shell`, no marks). Fields are `pub`
/// on `SessionInfo`, so a caller wanting a headline/color/label just sets them.
pub fn sess(name: &str) -> SessionInfo {
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

pub async fn connect(
    url: &str,
    token: Option<&str>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = url.into_client_request().unwrap();
    if let Some(t) = token {
        req.headers_mut()
            .insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

pub async fn send(ws: &mut Ws, msg: &AgentMsg, payload: &[u8]) {
    ws.send(Message::Binary(encode_frame(msg, payload))).await.unwrap();
}

/// Spawn a fake agent that registers `machine_id`, advertises one session, and
/// answers attaches with an RIS snapshot then echoes input as output.
///
/// Verbatim hoist of the former `e2e.rs::spawn_fake_agent`.
pub async fn spawn_fake_agent(hub: &str, machine_id: &str, token: Option<&str>, session: &str) {
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
            caps: vec![CAP_ASSISTANT_UPDATE.to_string(), CAP_ASSISTANT_INSTALL.to_string()],
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
                    send(&mut ws, &AgentMsg::Snapshot { ch }, b"\x1bcHELLO_FROM_AGENT").await;
                }
                HubMsg::Input { ch } => {
                    let bytes = payload.to_vec();
                    send(&mut ws, &AgentMsg::Output { ch }, &bytes).await;
                }
                HubMsg::Command { req, cmd } => {
                    let result = match cmd {
                        Cmd::UpdateAssistants { install_missing, .. } => CmdResult::Json(
                            serde_json::to_value(UpdateJob {
                                id: format!("install={install_missing}"),
                                phase: "done".into(),
                                ..Default::default()
                            })
                            .unwrap(),
                        ),
                        Cmd::UpdateStatus => CmdResult::Json(
                            serde_json::to_value(UpdateJob {
                                phase: "done".into(),
                                ..Default::default()
                            })
                            .unwrap(),
                        ),
                        _ => CmdResult::Ok,
                    };
                    send(&mut ws, &AgentMsg::Reply { req, result }, b"").await;
                }
                HubMsg::Ping => send(&mut ws, &AgentMsg::Pong, b"").await,
                _ => {}
            }
        }
    });
}

/// Read binary WS frames until one satisfies `pred` (or time out).
pub async fn read_until<F: Fn(&[u8]) -> bool>(ws: &mut Ws, pred: F) -> bool {
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

/// Like `spawn_fake_agent` but the attach snapshot carries the machine id, so a
/// test can tell *which* agent served an attach — needed to prove routing when
/// two machines own a session of the same name.
pub async fn spawn_fake_agent_marked(hub: &str, machine_id: &str, session: &str) {
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
            caps: vec![CAP_ASSISTANT_UPDATE.to_string()],
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

/// Wait for the uplink to register before routing anything to it.
pub async fn await_machine(hub: &str) {
    for _ in 0..50 {
        let m: Vec<serde_json::Value> =
            reqwest::get(format!("http://{hub}/api/machines")).await.unwrap().json().await.unwrap();
        if !m.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("no machine registered");
}

// ── A scriptable fake agent for the TUI harness (proposal 0059 B3) ──────────────
//
// The verbatim doubles above are deliberately fixed-behaviour so the hub suite
// stays byte-for-byte. The TUI scenarios need more: a mutable session list (a
// session can vanish), and observability of what the client sent through the hub
// (input bytes, resizes, lifecycle `Cmd`s). `FakeAgent` provides exactly that
// while reusing the SAME encode/decode + message matching, so there is still one
// wire-contract double.

/// What a `FakeAgent` observed from clients (via the hub), for assertions.
#[derive(Default)]
pub struct AgentObserved {
    /// Raw input bytes echoed from clients, in arrival order.
    pub input: Vec<u8>,
    /// Every `(cols, rows)` the client(s) resized to.
    pub resizes: Vec<(u16, u16)>,
    /// Lifecycle/control `Cmd`s the hub routed here (as debug strings — enough to
    /// assert "a Create landed" / "a SetLabel with this label landed").
    pub cmds: Vec<Cmd>,
}

/// A handle to a running `FakeAgent`. `Clone` is cheap (all `Arc`).
#[derive(Clone)]
pub struct FakeAgentHandle {
    machine: String,
    sessions: Arc<Mutex<Vec<SessionInfo>>>,
    observed: Arc<Mutex<AgentObserved>>,
    /// Outbound frames to push to the hub out-of-band (e.g. `Sessions` after a
    /// mutation, or a `Closed` on a channel). Drained by the agent's IO task.
    outbox: tokio::sync::mpsc::UnboundedSender<(AgentMsg, Vec<u8>)>,
}

impl FakeAgentHandle {
    pub fn machine(&self) -> &str {
        &self.machine
    }

    /// Replace the advertised session list and push the new `Sessions` to the hub.
    pub fn set_sessions(&self, sessions: Vec<SessionInfo>) {
        *self.sessions.lock().unwrap() = sessions.clone();
        let _ = self.outbox.send((AgentMsg::Sessions { sessions }, Vec::new()));
    }

    /// Snapshot of what clients have sent through the hub so far.
    pub fn observed(&self) -> std::sync::MutexGuard<'_, AgentObserved> {
        self.observed.lock().unwrap()
    }

    /// True once at least one resize with these dims has been observed.
    pub fn saw_resize(&self, cols: u16, rows: u16) -> bool {
        self.observed.lock().unwrap().resizes.iter().any(|&r| r == (cols, rows))
    }
}

/// Spawn a scriptable fake agent. It answers attaches with an RIS snapshot whose
/// body is `\x1bcSNAP:<machine>:<session>` (so the TUI's pane renders a string a
/// test can grep for), echoes input, records resizes, and answers lifecycle
/// `Cmd`s (`Create`→Created, `Delete`→Ok + drops the session, `SetLabel`→the
/// relabelled `SessionInfo`, `Restorable`→`restorable`, `Restore`→Ok).
pub async fn spawn_scriptable_agent(
    hub: &str,
    machine_id: &str,
    token: Option<&str>,
    sessions: Vec<SessionInfo>,
) -> FakeAgentHandle {
    let url = format!("ws://{hub}/agent/ws");
    let mut ws = connect(&url, token).await.expect("agent connects");
    send(
        &mut ws,
        &AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: machine_id.into(),
            hostname: machine_id.into(),
            agent_version: "test".into(),
            tools: vec![ToolInfo {
                cmd: "cc".into(),
                prefix: "claude".into(),
                extra_dirs: None,
                unavailable: false,
            }],
            caps: vec![CAP_ASSISTANT_UPDATE.to_string(), CAP_ASSISTANT_INSTALL.to_string()],
        },
        b"",
    )
    .await;
    send(&mut ws, &AgentMsg::Sessions { sessions: sessions.clone() }, b"").await;

    let sessions = Arc::new(Mutex::new(sessions));
    let observed = Arc::new(Mutex::new(AgentObserved::default()));
    let (otx, mut orx) = tokio::sync::mpsc::unbounded_channel::<(AgentMsg, Vec<u8>)>();
    let handle = FakeAgentHandle {
        machine: machine_id.to_string(),
        sessions: sessions.clone(),
        observed: observed.clone(),
        outbox: otx,
    };
    let machine = machine_id.to_string();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                // Out-of-band frames requested via the handle (Sessions/Closed/…).
                Some((msg, payload)) = orx.recv() => {
                    send(&mut ws, &msg, &payload).await;
                }
                incoming = ws.next() => {
                    let Some(Ok(Message::Binary(buf))) = incoming else { break };
                    let Ok((hub_msg, payload)) = decode_frame::<HubMsg>(&buf) else { continue };
                    match hub_msg {
                        HubMsg::Attach { ch, session, .. } => {
                            let snap = format!("\x1bcSNAP:{machine}:{session}");
                            send(&mut ws, &AgentMsg::Snapshot { ch }, snap.as_bytes()).await;
                        }
                        HubMsg::Input { ch } => {
                            observed.lock().unwrap().input.extend_from_slice(&payload);
                            let bytes = payload.to_vec();
                            send(&mut ws, &AgentMsg::Output { ch }, &bytes).await;
                        }
                        HubMsg::Resize { cols, rows, .. } => {
                            observed.lock().unwrap().resizes.push((cols, rows));
                        }
                        HubMsg::Command { req, cmd } => {
                            observed.lock().unwrap().cmds.push(cmd.clone());
                            let result = handle_cmd(&cmd, &sessions);
                            send(&mut ws, &AgentMsg::Reply { req, result }, b"").await;
                        }
                        HubMsg::Ping => send(&mut ws, &AgentMsg::Pong, b"").await,
                        _ => {}
                    }
                }
                else => break,
            }
        }
    });

    handle
}

fn handle_cmd(cmd: &Cmd, sessions: &Arc<Mutex<Vec<SessionInfo>>>) -> CmdResult {
    match cmd {
        Cmd::Create(req) => {
            // Register the created session so a following poll lists it.
            let name = if req.name.is_empty() { "new-session".to_string() } else { req.name.clone() };
            let mut s = sess(&name);
            s.tool = req.tool.clone();
            s.cwd = req.dir.clone();
            s.skip_permissions = Some(req.skip_permissions);
            sessions.lock().unwrap().push(s);
            CmdResult::Created(name)
        }
        Cmd::Delete(req) => {
            sessions.lock().unwrap().retain(|s| s.name != req.session);
            CmdResult::Ok
        }
        Cmd::SetLabel { session, label } => {
            let mut guard = sessions.lock().unwrap();
            let updated = guard.iter_mut().find(|s| &s.name == session).map(|s| {
                s.label = label.clone().filter(|l| !l.is_empty());
                s.clone()
            });
            match updated {
                Some(s) => CmdResult::Json(serde_json::to_value(s).unwrap()),
                None => CmdResult::Ok,
            }
        }
        Cmd::SetColor { session, color } => {
            let mut guard = sessions.lock().unwrap();
            let updated = guard.iter_mut().find(|s| &s.name == session).map(|s| {
                s.color = color.clone();
                s.clone()
            });
            match updated {
                Some(s) => CmdResult::Json(serde_json::to_value(s).unwrap()),
                None => CmdResult::Ok,
            }
        }
        Cmd::Restorable => {
            // A canned restorable set the picker can render (proposal 0059 C5).
            let mk = |name: &str| RestorableSession {
                session: name.into(),
                tool: "cc".into(),
                short: name.into(),
                dir: "~/work".into(),
            };
            CmdResult::Restorable(vec![mk("restore-me"), mk("restore-too")])
        }
        Cmd::Restore => CmdResult::Ok,
        _ => CmdResult::Ok,
    }
}
