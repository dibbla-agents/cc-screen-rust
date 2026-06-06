//! The agent side of the hub: accept an agent's outbound WebSocket at
//! `/agent/ws`, authenticate it by its per-agent token, and run the relay. The
//! agent's `Sessions` updates the registry; its per-channel `Snapshot`/`Output`/
//! `Closed` frames are routed to the matching browser bridge. On disconnect the
//! agent is greyed (offline, list retained) and its bridged browsers are closed.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use cc_screen_protocol::hub::{decode_frame, encode_frame, AgentMsg, HubMsg, HUB_PROTO_VERSION};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::registry::ToBrowser;
use crate::state::HubState;

/// `GET /agent/ws` — the agent uplink. Rejects an implausible token before the
/// upgrade; the exact `(machine, token)` check happens after `Register`.
pub async fn agent_ws(State(hub): State<HubState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let token = bearer(&headers);
    if !hub.handshake_token_plausible(token) {
        return (StatusCode::UNAUTHORIZED, "bad agent token").into_response();
    }
    let token = token.map(str::to_string);
    ws.on_upgrade(move |socket| serve_agent(hub, socket, token))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

async fn serve_agent(hub: HubState, socket: WebSocket, token: Option<String>) {
    let (mut ws_write, mut ws_read) = socket.split();

    // The first frame must be Register; it gates the (machine, token) pairing.
    let (machine_id, hostname, tools) = match ws_read.next().await {
        Some(Ok(Message::Binary(buf))) => match decode_frame::<AgentMsg>(&buf) {
            Ok((AgentMsg::Register { proto, machine_id, hostname, tools, .. }, _)) => {
                if proto != HUB_PROTO_VERSION {
                    tracing::warn!("agent {machine_id}: proto {proto} != {HUB_PROTO_VERSION}; closing");
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                }
                if !hub.uplink_token_ok_for(&machine_id, token.as_deref()) {
                    tracing::warn!("agent {machine_id}: rejected (bad uplink token)");
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                }
                // Fix 4: in OPEN uplink mode (no per-agent tokens) any peer could
                // register as an existing machine_id and silently displace the live
                // agent — a stealthy MITM of all its terminal/file traffic. Refuse a
                // duplicate while the original is still online. (Configured mode is
                // already gated: only the real agent holds the matching token, so a
                // genuine reconnect is allowed to replace.) A dropped agent is marked
                // offline, so a real reconnect after the drop still succeeds.
                if hub.agent_tokens.is_empty() && hub.registry.is_online(&machine_id) {
                    tracing::warn!("agent {machine_id}: rejected — already online (open-uplink takeover blocked)");
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                }
                (machine_id, hostname, tools)
            }
            _ => {
                tracing::warn!("agent uplink: first frame was not Register; closing");
                return;
            }
        },
        _ => return,
    };

    // Channel for everything the hub sends to this agent; a writer task owns the
    // WS sink and drains it.
    let (to_agent_tx, mut to_agent_rx) = mpsc::channel::<Vec<u8>>(1024);
    let conn = hub.registry.register(&machine_id, &hostname, tools, to_agent_tx);
    tracing::info!("agent {machine_id} registered ({hostname})");

    let writer = tokio::spawn(async move {
        while let Some(frame) = to_agent_rx.recv().await {
            if ws_write.send(Message::Binary(frame)).await.is_err() {
                break;
            }
        }
    });

    let mut ping = interval(Duration::from_secs(30));
    ping.tick().await; // consume the immediate first tick
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if conn.to_agent().send(encode_frame(&HubMsg::Ping, b"")).await.is_err() {
                    break;
                }
            }
            incoming = ws_read.next() => match incoming {
                Some(Ok(Message::Binary(buf))) => match decode_frame::<AgentMsg>(&buf) {
                    Ok((AgentMsg::Sessions { sessions }, _)) => conn.set_sessions(sessions),
                    // Terminal output for an attached client → its browser bridge.
                    Ok((AgentMsg::Snapshot { ch }, p)) | Ok((AgentMsg::Output { ch }, p)) => {
                        conn.route_to_browser(ch, ToBrowser::Bytes(p.to_vec())).await;
                    }
                    Ok((AgentMsg::Closed { ch }, _)) => {
                        conn.route_to_browser(ch, ToBrowser::Close).await;
                    }
                    // A filesystem-watch event → its browser bridge (sent as text).
                    Ok((AgentMsg::WatchEvt { ch }, p)) => {
                        conn.route_to_browser(ch, ToBrowser::Bytes(p.to_vec())).await;
                    }
                    // Reply to a routed control op → resolve the waiting handler.
                    Ok((AgentMsg::Reply { req, result }, _)) => conn.resolve_reply(req, result),
                    // Busy→waiting edge → centralized push, machine-stamped. Spawn
                    // so the (blocking) send doesn't stall this agent's read loop.
                    Ok((AgentMsg::WaitingEdge { session, short, preview }, _)) => {
                        let push = hub.push.clone();
                        let title = format!("{machine_id} · {short} is waiting");
                        let body = if preview.is_empty() {
                            "finished — tap to open".to_string()
                        } else {
                            preview
                        };
                        tokio::spawn(async move { push.notify(&title, &body, &session).await });
                    }
                    Ok((AgentMsg::Pong, _)) => {}
                    Ok((other, _)) => tracing::debug!("agent {machine_id}: unhandled {other:?}"),
                    Err(_) => tracing::warn!("agent {machine_id}: malformed frame (skipped)"),
                },
                Some(Ok(Message::Ping(_))) => {} // agent doesn't send WS pings; ignore
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::warn!("agent {machine_id}: ws error: {e}");
                    break;
                }
            },
        }
    }

    // Greyed: mark offline + close every bridged browser. The entry and its last
    // session list are retained for the UI.
    conn.go_offline();
    writer.abort();
    tracing::info!("agent {machine_id} offline");
}
