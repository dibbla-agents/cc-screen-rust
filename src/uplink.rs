//! The agent→hub uplink: dial out to a hub, register this machine, stream the
//! session list, and relay terminals. The agent keeps owning every PTY (the
//! engine is untouched); the uplink is a second, outbound transport sharing the
//! same `AppState`. A hub never owns a PTY — it relays.
//!
//! Each `Attach` from the hub spins up the SAME `attach_loop` the local axum
//! handler uses (`crate::attach`), so a hub-relayed client is just another
//! `register_client()` subscriber: the snapshot-first invariant, the per-client
//! min-size resize, and `Lagged`→resync all hold across the relay. Many channels
//! plus the control/poller traffic funnel through one WS writer task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use cc_screen_protocol::hub::{
    decode_frame, encode_frame, AgentMsg, ChannelId, HubMsg, HUB_PROTO_VERSION,
};
use cc_screen_protocol::SessionInfo;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::attach::{attach_loop, AttachOut, ClientEvent};
use crate::engine::{AppState, Session};

/// How often the agent re-checks its session list and pushes a delta to the hub.
const SESSIONS_POLL: Duration = Duration::from_secs(1);

/// A frame to write on the uplink WS: an encoded `AgentMsg`, or a WS-level Pong.
enum WsOut {
    Bin(Vec<u8>),
    Pong(Vec<u8>),
}

/// Reconnect backoff: 0.5s, 1s, 2s, 4s, 8s, then capped at 15s. Pure (no clock)
/// so it's unit-testable; the run loop resets `attempt` to 0 once connected, so a
/// healthy session that drops reconnects fast.
pub fn backoff(attempt: u32) -> Duration {
    let ms = 500u64.saturating_mul(1u64 << attempt.min(5));
    Duration::from_millis(ms.min(15_000))
}

/// Derive the agent-uplink WebSocket URL from the hub's base URL.
fn agent_ws_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws}/agent/ws")
}

/// Connect to `hub_url` and serve forever, reconnecting with backoff.
pub async fn run(state: AppState, hub_url: String, token: Option<String>, machine_id: String) {
    let url = agent_ws_url(&hub_url);
    tracing::info!("uplink: hub={url} machine_id={machine_id}");
    let mut attempt = 0u32;
    loop {
        match connect_and_serve(&state, &url, &hub_url, token.as_deref(), &machine_id).await {
            Err(e) => {
                tracing::warn!("uplink: connect failed: {e}");
                attempt = attempt.saturating_add(1);
            }
            Ok(()) => {
                tracing::info!("uplink: disconnected; reconnecting");
                attempt = 0;
            }
        }
        sleep(backoff(attempt)).await;
    }
}

/// Returns `Err` only if the handshake never completed (caller escalates backoff);
/// once connected it returns `Ok(())` on any disconnect.
async fn connect_and_serve(
    state: &AppState,
    url: &str,
    hub_base: &str,
    token: Option<&str>,
    machine_id: &str,
) -> anyhow::Result<()> {
    let mut req = url.into_client_request()?;
    if let Some(t) = token {
        let mut val = HeaderValue::from_str(&format!("Bearer {t}"))?;
        val.set_sensitive(true);
        req.headers_mut().insert(AUTHORIZATION, val);
    }
    let (ws, _resp) = tokio_tungstenite::connect_async(req).await?;
    let (mut ws_write, mut ws_read) = ws.split();

    // One writer task owns the WS sink; the poller, control replies, and every
    // attached channel's forwarder funnel frames through `out_tx`.
    let (out_tx, mut out_rx) = mpsc::channel::<WsOut>(1024);
    let writer = tokio::spawn(async move {
        while let Some(o) = out_rx.recv().await {
            let msg = match o {
                WsOut::Bin(b) => Message::Binary(b),
                WsOut::Pong(p) => Message::Pong(p),
            };
            if ws_write.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Register, then begin streaming the session list.
    let register = AgentMsg::Register {
        proto: HUB_PROTO_VERSION,
        machine_id: machine_id.to_string(),
        hostname: machine_id.to_string(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        tools: state.inner.tools.iter().map(crate::tools::tool_info).collect(),
    };
    let _ = out_tx.send(WsOut::Bin(encode_frame(&register, b""))).await;

    // Per-channel state (terminal + watch) for this connection.
    let mut chans = ChannelMaps { term: HashMap::new(), watch: HashMap::new() };
    let mut last: Vec<SessionInfo> = Vec::new();
    // Per-session `waiting` state, for emitting busy→waiting edges to the hub.
    let mut prev_waiting: HashMap<String, bool> = HashMap::new();
    let mut tick = interval(SESSIONS_POLL);

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let cur = crate::handlers::session_list(state);
                // Busy→waiting edges → WaitingEdge so the hub can buzz devices
                // subscribed to it (centralized push). Best-effort: a missed buzz
                // never breaks the session stream. First sight records state only
                // (no edge), so startup doesn't fire for already-idle sessions.
                for s in &cur {
                    let was = prev_waiting.insert(s.name.clone(), s.waiting);
                    if was == Some(false) && s.waiting {
                        let edge = AgentMsg::WaitingEdge {
                            session: s.name.clone(),
                            short: s.short.clone(),
                            preview: s.preview.clone(),
                        };
                        let _ = out_tx.send(WsOut::Bin(encode_frame(&edge, b""))).await;
                    }
                }
                prev_waiting.retain(|name, _| cur.iter().any(|s| &s.name == name));
                if cur != last {
                    last = cur.clone();
                    let frame = encode_frame(&AgentMsg::Sessions { sessions: cur }, b"");
                    if out_tx.send(WsOut::Bin(frame)).await.is_err() {
                        break;
                    }
                }
            }
            incoming = ws_read.next() => match incoming {
                Some(Ok(Message::Binary(buf))) => match decode_frame::<HubMsg>(&buf) {
                    Ok((msg, payload)) => {
                        if !handle_hub(state, hub_base, token, &out_tx, &mut chans, msg, payload).await {
                            break;
                        }
                    }
                    // Skip a malformed frame; keep the connection open.
                    Err(_) => tracing::warn!("uplink: malformed hub frame (skipped)"),
                },
                Some(Ok(Message::Ping(p))) => {
                    if out_tx.send(WsOut::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::warn!("uplink: read error: {e}");
                    break;
                }
            },
        }
    }

    // Disconnect: dropping every terminal channel's event sender ends its
    // attach_loop, which unregisters the client (releasing the PTY min-size pin).
    // Tear down watch channels explicitly (unregister + stop the forwarder).
    drop(chans.term);
    for (_, w) in chans.watch.drain() {
        state.inner.watcher.unregister(w.id);
        w.forwarder.abort();
    }
    writer.abort();
    Ok(())
}

/// A relayed filesystem-watch channel: the `Watcher` client id plus the forwarder
/// task that turns its events into `WatchEvt` frames.
struct WatchChan {
    id: u64,
    forwarder: tokio::task::JoinHandle<()>,
}

/// Everything a `HubMsg` handler may touch on this connection.
struct ChannelMaps {
    /// ch → terminal client-event sender (one `attach_loop` each).
    term: HashMap<ChannelId, mpsc::Sender<ClientEvent>>,
    /// ch → relayed fs-watch channel.
    watch: HashMap<ChannelId, WatchChan>,
}

/// Handle one `HubMsg`. Returns `false` only when the writer is gone (stop).
async fn handle_hub(
    state: &AppState,
    hub_base: &str,
    token: Option<&str>,
    out_tx: &mpsc::Sender<WsOut>,
    chans: &mut ChannelMaps,
    msg: HubMsg,
    payload: &[u8],
) -> bool {
    match msg {
        HubMsg::Attach { ch, session, cols, rows } => match state.get(&session) {
            Some(sess) => {
                chans.term.insert(ch, spawn_channel(sess, ch, cols, rows, out_tx.clone()));
            }
            None => {
                // Unknown session → tell the hub this channel is already closed.
                let _ = out_tx.send(WsOut::Bin(encode_frame(&AgentMsg::Closed { ch }, b""))).await;
            }
        },
        HubMsg::Input { ch } => {
            if let Some(tx) = chans.term.get(&ch) {
                let _ = tx.send(ClientEvent::Input(payload.to_vec())).await;
            }
        }
        HubMsg::Resize { ch, cols, rows } => {
            if let Some(tx) = chans.term.get(&ch) {
                let _ = tx.send(ClientEvent::Resize(cols, rows)).await;
            }
        }
        HubMsg::WatchSub { ch, dirs, unsub } => {
            // First sub on this ch opens a Watcher client + a forwarder.
            let id = match chans.watch.get(&ch) {
                Some(w) => w.id,
                None => {
                    let (id, mut rx) = state.inner.watcher.register();
                    let out = out_tx.clone();
                    let forwarder = tokio::spawn(async move {
                        while let Some(ev) = rx.recv().await {
                            let body = serde_json::json!({ "t": "fs", "dir": ev.dir, "paths": ev.paths });
                            let frame = encode_frame(&AgentMsg::WatchEvt { ch }, body.to_string().as_bytes());
                            if out.send(WsOut::Bin(frame)).await.is_err() {
                                break;
                            }
                        }
                    });
                    chans.watch.insert(ch, WatchChan { id, forwarder });
                    id
                }
            };
            for d in &dirs {
                if unsub {
                    state.inner.watcher.unsubscribe(id, d);
                } else {
                    state.inner.watcher.subscribe(id, d);
                }
            }
        }
        HubMsg::Detach { ch } => {
            // Dropping the terminal sender ends its attach_loop → unregister; a
            // watch channel is torn down explicitly.
            chans.term.remove(&ch);
            if let Some(w) = chans.watch.remove(&ch) {
                state.inner.watcher.unregister(w.id);
                w.forwarder.abort();
            }
        }
        HubMsg::Command { req, cmd } => {
            // Run the op against the local engine and reply (correlated by req).
            let result = crate::ops::run_cmd(state, cmd);
            let _ = out_tx.send(WsOut::Bin(encode_frame(&AgentMsg::Reply { req, result }, b""))).await;
        }
        HubMsg::Ping => {
            if out_tx.send(WsOut::Bin(encode_frame(&AgentMsg::Pong, b""))).await.is_err() {
                return false;
            }
        }
        HubMsg::OpenBulk { req, bulk } => {
            // Big transfers run on a fresh, dedicated WS (off the control channel).
            tokio::spawn(crate::bulk::serve(
                state.clone(),
                hub_base.to_string(),
                token.map(str::to_string),
                req,
                bulk,
            ));
        }
    }
    true
}

/// Spin up one client channel: an `attach_loop` against `sess` plus a forwarder
/// that encodes its `AttachOut` as `ch`-tagged `AgentMsg` frames. Returns the
/// channel's client-event sender (dropping it ends the channel).
fn spawn_channel(
    sess: Arc<Session>,
    ch: ChannelId,
    cols: u16,
    rows: u16,
    out_tx: mpsc::Sender<WsOut>,
) -> mpsc::Sender<ClientEvent> {
    let (ev_tx, ev_rx) = mpsc::channel::<ClientEvent>(256);
    let (ao_tx, mut ao_rx) = mpsc::channel::<AttachOut>(256);

    // The hub carries the client's initial size in Attach; apply it as the first
    // resize so the PTY min-size reconciliation sees this client.
    if cols > 0 && rows > 0 {
        let _ = ev_tx.try_send(ClientEvent::Resize(cols, rows));
    }
    tokio::spawn(attach_loop(sess, ao_tx, ev_rx));
    tokio::spawn(async move {
        while let Some(ao) = ao_rx.recv().await {
            let frame = match ao {
                AttachOut::Snapshot(b) => encode_frame(&AgentMsg::Snapshot { ch }, &b),
                AttachOut::Output(b) => encode_frame(&AgentMsg::Output { ch }, &b),
                AttachOut::Closed => encode_frame(&AgentMsg::Closed { ch }, b""),
            };
            if out_tx.send(WsOut::Bin(frame)).await.is_err() {
                break;
            }
        }
    });
    ev_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule_grows_and_caps() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_millis(1000));
        assert_eq!(backoff(2), Duration::from_millis(2000));
        assert_eq!(backoff(3), Duration::from_millis(4000));
        assert_eq!(backoff(4), Duration::from_millis(8000));
        // Caps at 15s and never exceeds it, even for absurd attempt counts.
        assert_eq!(backoff(5), Duration::from_millis(15_000));
        assert_eq!(backoff(100), Duration::from_millis(15_000));
        // Monotonic non-decreasing.
        let mut prev = backoff(0);
        for a in 1..40 {
            let cur = backoff(a);
            assert!(cur >= prev, "backoff must not decrease at attempt {a}");
            prev = cur;
        }
    }

    #[test]
    fn agent_ws_url_swaps_scheme_and_appends_path() {
        assert_eq!(agent_ws_url("http://hub:8840"), "ws://hub:8840/agent/ws");
        assert_eq!(agent_ws_url("https://hub.ts.net"), "wss://hub.ts.net/agent/ws");
        assert_eq!(agent_ws_url("http://hub:8840/"), "ws://hub:8840/agent/ws");
    }
}
