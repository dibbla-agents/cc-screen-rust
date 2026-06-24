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
use tokio::time::{interval, sleep, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::attach::{attach_loop, AttachOut, ClientEvent};
use crate::engine::{notification_eligible, now_secs, AppState, Session};

/// How often the agent re-checks its session list and pushes a delta to the hub.
const SESSIONS_POLL: Duration = Duration::from_secs(1);

/// If the agent hears *nothing* from the hub for this long, it treats the uplink
/// as a silently half-open (dead) connection and drops it so the run loop
/// reconnects. The hub pings every 30s (`crates/hub` `uplink_server`), so any
/// healthy link refreshes well inside this window; 60s is a 2× margin. Without
/// this watchdog an idle agent behind a dropped tunnel never notices the death —
/// it only ever *writes* on a session-list delta, so an idle fleet writes nothing,
/// the dead socket is never exercised, and the agent zombies while the hub has
/// long since marked it offline.
const HUB_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the watchdog re-checks the idle deadline above.
const HEARTBEAT_CHECK: Duration = Duration::from_secs(15);

/// Cap on a single connect attempt (TCP + TLS + WS upgrade). Without it,
/// `connect_async` can block forever when a handshake stalls half-open (seen
/// through Cloudflare after a `Connection reset`): the reconnect loop then hangs
/// silently and the agent vanishes from the hub until restarted. With it, a
/// stalled connect becomes an error → the run loop logs it and retries with
/// backoff. Comfortably above a normal connect, well under the 60s idle watchdog.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

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

/// Agent-side summary knobs (proposal 0022): how big a terminal tail to extract,
/// and how often to evaluate candidacy. The hub owns the actual spend gate.
#[derive(Clone, Copy)]
pub struct SummaryParams {
    pub tail_lines: usize,
    pub interval_secs: u64,
}

/// Connect to `hub_url` and serve forever, reconnecting with backoff.
pub async fn run(
    state: AppState,
    hub_url: String,
    token: Option<String>,
    machine_id: String,
    summary: SummaryParams,
) {
    let url = agent_ws_url(&hub_url);
    tracing::info!("uplink: hub={url} machine_id={machine_id}");
    let mut attempt = 0u32;
    loop {
        match connect_and_serve(&state, &url, &hub_url, token.as_deref(), &machine_id, summary).await {
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
    summary: SummaryParams,
) -> anyhow::Result<()> {
    let mut req = url.into_client_request()?;
    if let Some(t) = token {
        let mut val = HeaderValue::from_str(&format!("Bearer {t}"))?;
        val.set_sensitive(true);
        req.headers_mut().insert(AUTHORIZATION, val);
    }
    // Bounded connect: a stalled handshake must not wedge the reconnect loop.
    let (ws, _resp) = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req)).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("connect timed out after {}s", CONNECT_TIMEOUT.as_secs()),
    };
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
    // Periodic summary candidacy (proposal 0022). Floor at 5s so a misconfigured
    // tiny value can't hot-loop the LLM.
    let mut summary_tick = interval(Duration::from_secs(summary.interval_secs.max(5)));
    // Half-open watchdog: every frame from the hub (incl. its 30s ping) bumps
    // `last_recv`; if `heartbeat` ever finds it stale past HUB_IDLE_TIMEOUT, the
    // path is dead and we reconnect. Registration just happened, so seed it now.
    let mut heartbeat = interval(HEARTBEAT_CHECK);
    let mut last_recv = Instant::now();

    loop {
        tokio::select! {
            _ = tick.tick() => {
                let cur = crate::handlers::session_list(state);
                let now = now_secs();
                // Busy→waiting edges → WaitingEdge so the hub can buzz devices
                // subscribed to it (centralized push). Best-effort: a missed buzz
                // never breaks the session stream. First sight records state only
                // (no edge), so startup doesn't fire for already-idle sessions.
                for s in &cur {
                    let was = prev_waiting.insert(s.name.clone(), s.waiting);
                    if was == Some(false)
                        && s.waiting
                        && notification_eligible(s.busy_since, s.last_input_at, now)
                    {
                        // The push carries the agent's last cached summary detail
                        // (proposal 0022) so the buzz says what's needed; the hub
                        // falls back to preview when it's absent.
                        let edge = AgentMsg::WaitingEdge {
                            session: s.name.clone(),
                            short: s.short.clone(),
                            preview: s.preview.clone(),
                            detail: s.detail.clone(),
                        };
                        let _ = out_tx.send(WsOut::Bin(encode_frame(&edge, b""))).await;
                        // The session just finished a turn — ask for a fresh summary
                        // now so the next poll / next push reflects this state.
                        request_summary(state, &out_tx, machine_id, &s.name, summary.tail_lines).await;
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
            _ = summary_tick.tick() => {
                // Steady-state candidacy sweep: a changed (hash differs) session
                // that isn't already in flight gets one SummaryRequest. Idle
                // sessions send nothing (the hash gate is local + cheap).
                for sess in state.list() {
                    request_summary(state, &out_tx, machine_id, &sess.name, summary.tail_lines).await;
                }
            }
            _ = heartbeat.tick() => {
                // The hub pings every 30s; if we've heard nothing for HUB_IDLE_TIMEOUT
                // the link is silently half-open (our socket may still read ESTABLISHED
                // while the path is dead). Drop it — the run loop reconnects with backoff.
                if last_recv.elapsed() > HUB_IDLE_TIMEOUT {
                    tracing::warn!(
                        "uplink: no frame from hub in {}s; assuming dead, reconnecting",
                        HUB_IDLE_TIMEOUT.as_secs()
                    );
                    break;
                }
            }
            incoming = ws_read.next() => {
                // Any frame at all (a HubMsg, the 30s ping, a WS ping) proves the
                // path is live — refresh the watchdog before dispatching.
                last_recv = Instant::now();
                match incoming {
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
                }
            }
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

/// One relayed terminal channel: its client-event sender. Hub-relayed input
/// always reaches the PTY writer, exactly as a direct client's does — view-only
/// no longer exists (0014).
struct TermChan {
    tx: mpsc::Sender<ClientEvent>,
}

/// Everything a `HubMsg` handler may touch on this connection.
struct ChannelMaps {
    /// ch → relayed terminal channel (one `attach_loop` each).
    term: HashMap<ChannelId, TermChan>,
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
                let tx = spawn_channel(sess, ch, cols, rows, out_tx.clone());
                chans.term.insert(ch, TermChan { tx });
            }
            None => {
                // Unknown session → tell the hub this channel is already closed.
                let _ = out_tx.send(WsOut::Bin(encode_frame(&AgentMsg::Closed { ch }, b""))).await;
            }
        },
        HubMsg::Input { ch } => {
            if let Some(tc) = chans.term.get(&ch) {
                // Hub-relayed input always reaches the PTY, exactly as a direct
                // client's does (0014 removed the view-only drop).
                let _ = tc.tx.send(ClientEvent::Input(payload.to_vec())).await;
            }
        }
        HubMsg::Resize { ch, cols, rows } => {
            if let Some(tc) = chans.term.get(&ch) {
                let _ = tc.tx.send(ClientEvent::Resize(cols, rows)).await;
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
        HubMsg::SummaryResult { session, content_hash, headline, detail } => {
            // The hub answered (or declined). Cache only a non-declined result that
            // still matches the latest requested hash (else it's stale). A declined
            // result (both None) leaves the cache untouched — clients keep showing
            // preview. (0022 §5.)
            if let (Some(h), Some(d)) = (headline, detail) {
                if let Some(sess) = state.get(&session) {
                    let stored = sess.store_summary(content_hash, h, d);
                    tracing::debug!("summary result for {session}: stored={stored}");
                }
            }
        }
        HubMsg::Ping => {
            if out_tx.send(WsOut::Bin(encode_frame(&AgentMsg::Pong, b""))).await.is_err() {
                return false;
            }
        }
        HubMsg::OpenBulk { id, bulk } => {
            // Big transfers run on a fresh, dedicated WS (off the control channel).
            // We present our machine_id + the nonce on the dial-back so the hub can
            // bind the slot to this machine.
            tokio::spawn(crate::bulk::serve(
                state.clone(),
                hub_base.to_string(),
                token.map(str::to_string),
                id,
                bulk,
            ));
        }
    }
    true
}

/// If `session` is a summary candidate (its content changed since the last
/// summary and no request is in flight), redact + extract its inputs/tail and send
/// a `SummaryRequest` to the hub, marking the hash in-flight. A no-op for an
/// unchanged or already-requested session — so an idle fleet sends nothing.
async fn request_summary(
    state: &AppState,
    out_tx: &mpsc::Sender<WsOut>,
    machine_id: &str,
    session: &str,
    tail_lines: usize,
) {
    let Some(sess) = state.get(session) else { return };
    let (hash, inputs, tail) = sess.summary_extract(tail_lines);
    if !sess.summary_candidate(hash) {
        return;
    }
    sess.mark_summary_requested(hash);
    let req = AgentMsg::SummaryRequest {
        machine: machine_id.to_string(),
        session: session.to_string(),
        content_hash: hash,
        inputs,
        tail,
    };
    let _ = out_tx.send(WsOut::Bin(encode_frame(&req, b""))).await;
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
