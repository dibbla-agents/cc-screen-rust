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
use cc_screen_protocol::hub::{
    decode_frame, encode_frame, AgentMsg, HubMsg, HUB_PROTO_VERSION, MIN_SUPPORTED_PROTO,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::registry::ToBrowser;
use crate::state::HubState;

/// `GET /agent/ws` — the agent uplink. Rejects an implausible token before the
/// upgrade; the exact `(machine, token)` check happens after `Register`.
pub async fn agent_ws(State(hub): State<HubState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    // Runtime backstop (proposal 0010, Part 3): in unguarded open-uplink mode (no
    // per-agent tokens and no explicit CCHUB_ALLOW_OPEN_UPLINK opt-in), refuse a
    // registration that arrived through a reverse proxy. Forwarded headers mean the
    // connection did NOT originate on this host — exactly the loopback-behind-a-
    // tunnel case the startup bind-scope check can't see. A genuine local dev peer
    // carries no such headers; one that forges them is already local and gains
    // nothing. This is defense-in-depth behind the startup guard, not the primary
    // control (per-agent tokens are).
    if hub.open_uplink_unguarded() && proxied(&headers) {
        tracing::warn!("agent uplink: refused open-uplink registration arriving through a proxy");
        return (
            StatusCode::FORBIDDEN,
            "open uplink is not allowed through a proxy: set CCHUB_AGENT_TOKENS to \
             gate the uplink, or CCHUB_ALLOW_OPEN_UPLINK=1 to allow it anyway",
        )
            .into_response();
    }
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

/// True when the request carries reverse-proxy markers, i.e. it was forwarded
/// rather than originating on this host. Behind cloudflared / a TLS proxy these
/// are set and sanitized by the edge, so they're trustworthy there.
fn proxied(headers: &HeaderMap) -> bool {
    const MARKERS: [&str; 3] = ["x-forwarded-for", "cf-connecting-ip", "forwarded"];
    MARKERS.iter().any(|h| headers.contains_key(*h))
}

async fn serve_agent(hub: HubState, socket: WebSocket, token: Option<String>) {
    let (mut ws_write, mut ws_read) = socket.split();

    // The first frame must be Register; it gates the (machine, token) pairing and
    // resolves the owning tenant.
    let (machine_id, user_id, agent_id, hostname, tools) = match ws_read.next().await {
        Some(Ok(Message::Binary(buf))) => match decode_frame::<AgentMsg>(&buf) {
            Ok((AgentMsg::Register { proto, machine_id, hostname, tools, .. }, _)) => {
                // Negotiate a version *range* instead of demanding exact equality
                // (proposal 0001 §9.3): accept any agent in
                // [MIN_SUPPORTED_PROTO, HUB_PROTO_VERSION] so a staggered fleet
                // rollout interoperates in both directions, then operate at the
                // lower of the two — the highest shape both peers understand.
                if proto < MIN_SUPPORTED_PROTO || proto > HUB_PROTO_VERSION {
                    tracing::warn!(
                        "agent {machine_id}: proto {proto} outside supported \
                         [{MIN_SUPPORTED_PROTO}, {HUB_PROTO_VERSION}]; closing"
                    );
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                }
                let _negotiated = proto.min(HUB_PROTO_VERSION);
                // Resolve (machine, token) to its owning (user_id, agent_id) via the
                // §9.1 seam: single-tenant → (OWNER, machine_id); multi-tenant → the
                // DB row. `None` ⇒ reject (bad/absent token).
                let Some((user_id, agent_id)) = hub.resolve_agent(&machine_id, token.as_deref()).await
                else {
                    tracing::warn!("agent {machine_id}: rejected (bad uplink token)");
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                };
                // Fix 4: in OPEN uplink mode (no per-agent tokens) any peer could
                // register as an existing machine_id and silently displace the live
                // agent — a stealthy MITM of all its terminal/file traffic. Refuse a
                // duplicate while the original is still online. This applies only to
                // single-tenant open mode; configured single-tenant AND multi-tenant
                // are token-gated, so a genuine reconnect (same token) may replace. A
                // dropped agent is marked offline, so a real reconnect still succeeds.
                if !hub.multi_tenant() && hub.agent_tokens.is_empty() && hub.registry.is_online(&agent_id) {
                    tracing::warn!("agent {machine_id}: rejected — already online (open-uplink takeover blocked)");
                    let _ = ws_write.send(Message::Close(None)).await;
                    return;
                }
                (machine_id, user_id, agent_id, hostname, tools)
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
    let conn = hub.registry.register_agent(&agent_id, &user_id, &machine_id, &hostname, tools, to_agent_tx);
    tracing::info!("agent {machine_id} registered ({hostname}) [agent_id={agent_id} user={user_id}]");

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
                    // The push body prefers the agent's cached LLM summary detail
                    // (proposal 0022), falling back to preview, then a nudge.
                    Ok((AgentMsg::WaitingEdge { session, short, preview, detail }, _)) => {
                        let push = hub.push.clone();
                        let title = format!("{machine_id} · {short} is waiting");
                        let body = detail
                            .filter(|d| !d.is_empty())
                            .or_else(|| Some(preview).filter(|p| !p.is_empty()))
                            .unwrap_or_else(|| "finished — tap to open".to_string());
                        tokio::spawn(async move { push.notify(&title, &body, &session).await });
                    }
                    // Agent asks for a session summary (proposal 0022). Gate + call
                    // Haiku off the read loop, then send the result back over the
                    // uplink (echoing content_hash). A declined/failed call returns
                    // None/None so the agent keeps showing preview.
                    Ok((AgentMsg::SummaryRequest { session, content_hash, inputs, tail, .. }, _)) => {
                        let summary = hub.summary.clone();
                        let to_agent = conn.to_agent().clone();
                        tokio::spawn(async move {
                            let (headline, detail) = match summary.summarize(&inputs, &tail).await {
                                crate::summarizer::Outcome::Ok(s) => (Some(s.headline), Some(s.detail)),
                                _ => (None, None),
                            };
                            let frame = encode_frame(
                                &HubMsg::SummaryResult { session, content_hash, headline, detail },
                                b"",
                            );
                            let _ = to_agent.send(frame).await;
                        });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn proxied_detects_forwarded_markers() {
        // A direct (loopback/tailnet) connection carries no forwarded headers.
        assert!(!proxied(&hdrs(&[("authorization", "Bearer x")])));
        assert!(!proxied(&hdrs(&[])));
        // Each reverse-proxy marker trips it.
        assert!(proxied(&hdrs(&[("x-forwarded-for", "1.2.3.4")])));
        assert!(proxied(&hdrs(&[("cf-connecting-ip", "1.2.3.4")])));
        assert!(proxied(&hdrs(&[("forwarded", "for=1.2.3.4")])));
        // Header name matching is case-insensitive (HeaderMap normalizes).
        assert!(proxied(&hdrs(&[("X-Forwarded-For", "1.2.3.4")])));
    }
}
