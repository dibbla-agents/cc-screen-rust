//! The filesystem-watch bridge: `GET /api/watch?machine=`. Like the terminal
//! bridge, but the payload is the editor's `{t:"fs",dir,paths}` JSON (sent as text
//! frames), and client `{t:"sub"|"unsub",dirs}` frames become `WatchSub` ops on
//! the owning agent's `Watcher`.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use cc_screen_protocol::hub::{encode_frame, HubMsg};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::registry::{AgentConn, ToBrowser, UserScope};
use crate::state::HubState;
use axum::Extension;

#[derive(Deserialize)]
pub struct WatchQuery {
    #[serde(default)]
    machine: String,
}

#[derive(Deserialize)]
struct WatchFrame {
    t: String,
    #[serde(default)]
    dirs: Vec<String>,
}

pub async fn ws(
    State(hub): State<HubState>,
    Extension(scope): Extension<UserScope>,
    Query(q): Query<WatchQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !hub.origin.check(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }
    // Watch has no session to disambiguate by; resolve (within the caller's tenant)
    // to the single online machine when the client (the PWA) omits `machine`.
    let Some(agent) = hub.registry.resolve_scoped(&scope, &q.machine, None) else {
        return (StatusCode::NOT_FOUND, "specify ?machine= (more than one is online)").into_response();
    };
    ws.on_upgrade(move |socket| bridge(agent, socket))
}

async fn bridge(agent: Arc<AgentConn>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (to_browser_tx, mut to_browser_rx) = mpsc::channel::<ToBrowser>(256);
    let ch = agent.open_channel(to_browser_tx);

    // Writer: agent fs-event JSON → text frames; + a keepalive ping.
    let mut send_task = tokio::spawn(async move {
        let mut ping = interval(Duration::from_secs(30));
        ping.tick().await;
        loop {
            tokio::select! {
                m = to_browser_rx.recv() => match m {
                    Some(ToBrowser::Bytes(b)) => {
                        let text = String::from_utf8_lossy(&b).into_owned();
                        if sink.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Some(ToBrowser::Close) | None => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Reader: client sub/unsub → WatchSub ops on the agent.
    let agent_r = agent.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(f) = serde_json::from_str::<WatchFrame>(&t) {
                        let unsub = match f.t.as_str() {
                            "sub" => false,
                            "unsub" => true,
                            _ => continue,
                        };
                        let frame = encode_frame(&HubMsg::WatchSub { ch, dirs: f.dirs, unsub }, b"");
                        if agent_r.to_agent().send(frame).await.is_err() {
                            break;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    // Detach tears down the agent's Watcher client for this ch.
    let _ = agent.to_agent().send(encode_frame(&HubMsg::Detach { ch }, b"")).await;
    agent.close_channel(ch);
}
