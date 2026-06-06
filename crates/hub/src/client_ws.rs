//! The client↔agent terminal bridge: `GET /api/ws?machine=&session=`. Allocates a
//! channel on the owning agent, sends `Attach`, then splices the browser WS to
//! that channel — agent→browser bytes go out verbatim (the hub never parses
//! terminal output), browser→agent input/resize become `HubMsg` frames. The
//! browser maps 1:1 to one `register_client()` on the agent, so every engine
//! invariant (snapshot-first, min-size resize, `Lagged`→resync) holds across the
//! relay.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use cc_screen_protocol::hub::{encode_frame, HubMsg};
use cc_screen_protocol::WsClientFrame;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::registry::{AgentConn, ToBrowser};
use crate::state::HubState;

#[derive(Deserialize)]
pub struct WsQuery {
    /// Optional: a client that doesn't thread it (the React PWA) omits it, and the
    /// hub resolves the owning machine from the session name.
    #[serde(default)]
    machine: String,
    session: String,
}

pub async fn ws(
    State(hub): State<HubState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !hub.origin.check(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }
    let Some(agent) = hub.registry.resolve(&q.machine, Some(&q.session)) else {
        return (StatusCode::NOT_FOUND, "no online machine for that session").into_response();
    };
    ws.on_upgrade(move |socket| bridge(agent, q.session, socket))
}

async fn bridge(agent: Arc<AgentConn>, session: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (to_browser_tx, mut to_browser_rx) = mpsc::channel::<ToBrowser>(256);
    let ch = agent.open_channel(to_browser_tx);

    // Ask the agent to attach. Initial size 0,0 — the browser sends a resize frame
    // immediately after connecting, which we relay as HubMsg::Resize.
    let attach = encode_frame(&HubMsg::Attach { ch, session, cols: 0, rows: 0 }, b"");
    if agent.to_agent().send(attach).await.is_err() {
        agent.close_channel(ch);
        return;
    }

    // Writer: agent→browser bytes + a 30s keepalive ping.
    let mut send_task = tokio::spawn(async move {
        let mut ping = interval(Duration::from_secs(30));
        ping.tick().await;
        loop {
            tokio::select! {
                m = to_browser_rx.recv() => match m {
                    Some(ToBrowser::Bytes(b)) => {
                        if sink.send(Message::Binary(b)).await.is_err() {
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

    // Reader: browser→agent input/resize.
    let agent_r = agent.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let frame = match msg {
                Message::Text(t) => match serde_json::from_str::<WsClientFrame>(&t) {
                    Ok(m) => match m.t.as_str() {
                        "i" => Some(encode_frame(&HubMsg::Input { ch }, m.d.as_bytes())),
                        "r" => Some(encode_frame(&HubMsg::Resize { ch, cols: m.c, rows: m.r }, b"")),
                        _ => None,
                    },
                    Err(_) => None,
                },
                Message::Binary(b) => Some(encode_frame(&HubMsg::Input { ch }, &b)),
                Message::Close(_) => break,
                _ => None,
            };
            if let Some(f) = frame {
                if agent_r.to_agent().send(f).await.is_err() {
                    break;
                }
            }
        }
    });

    // Whichever side ends, tear down the other, then Detach so the agent's
    // attach_loop unregisters its client (releasing the PTY min-size pin).
    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
    let _ = agent.to_agent().send(encode_frame(&HubMsg::Detach { ch }, b"")).await;
    agent.close_channel(ch);
}
