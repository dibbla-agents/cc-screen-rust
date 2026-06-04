//! The transport-agnostic attach loop.
//!
//! The heart of attaching a client to a session — the atomic snapshot+subscribe
//! (`Session::attach`), the broadcast pump with `Lagged`→resync, the per-client
//! resize, and the always-runs `unregister_client` on exit — lives here so BOTH
//! the local axum WebSocket handler (`handlers::ws`) and the hub uplink
//! (`uplink.rs`) drive the *identical* engine path. The engine (`engine.rs`)
//! cannot tell a directly-attached browser from a hub-relayed client: each is one
//! `register_client()` subscriber, and the only difference is the transport
//! carrying these channel messages.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::engine::Session;

/// Engine → client. The transport turns these into WebSocket frames (locally) or
/// `AgentMsg` frames on the uplink (`Snapshot`/`Output`/`Closed`).
#[derive(Debug)]
pub enum AttachOut {
    /// A clean, size-agnostic repaint (RIS-prefixed). ALWAYS the first frame on
    /// attach, and the resync after a slow client lags the broadcast ring.
    Snapshot(Vec<u8>),
    /// Live PTY bytes.
    Output(Vec<u8>),
    /// The session's child exited — the transport should close the client.
    Closed,
}

/// Client → engine. Dropping the sender (so `recv()` yields `None`) signals that
/// the client detached.
#[derive(Debug)]
pub enum ClientEvent {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// Drive one client's attachment against `sess` until the client detaches (its
/// `ev` sender drops), the session closes, or the `out` sink dies. Registers the
/// client on entry and ALWAYS unregisters on exit, so the PTY's per-axis min-size
/// pin is released and no phantom client lingers.
pub async fn attach_loop(
    sess: Arc<Session>,
    out: mpsc::Sender<AttachOut>,
    mut ev: mpsc::Receiver<ClientEvent>,
) {
    // Atomic snapshot + subscribe, then register (the PTY pins to the per-axis
    // min across attached clients). The snapshot is the FIRST frame — the engine
    // guarantees no byte is both replayed and streamed.
    let (snap, mut rx) = sess.attach();
    let mut closed_rx = sess.closed_rx();
    let client = sess.register_client();

    if out.send(AttachOut::Snapshot(snap)).await.is_err() {
        sess.unregister_client(client);
        return;
    }

    // If the child already exited before we subscribed, `closed_rx.changed()` is
    // edge-triggered and would never fire — surface the close now. (Past this
    // point a real exit is a fresh change and the select arm catches it.)
    if *closed_rx.borrow() {
        let _ = out.send(AttachOut::Closed).await;
        sess.unregister_client(client);
        return;
    }

    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Ok(b) => {
                    if out.send(AttachOut::Output(b.to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Slow client fell behind the ring → resync with a fresh repaint.
                    if out.send(AttachOut::Snapshot(sess.snapshot())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break, // broadcast closed
            },
            event = ev.recv() => match event {
                Some(ClientEvent::Input(b)) => sess.write_input(&b),
                Some(ClientEvent::Resize(c, r)) => sess.resize_client(client, c, r),
                None => break, // client detached
            },
            _ = closed_rx.changed() => {
                let _ = out.send(AttachOut::Closed).await;
                break;
            }
        }
    }

    sess.unregister_client(client);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::AppState;
    use crate::tools::Tool;
    use std::time::Duration;

    // The engine's initial PTY geometry (engine.rs INIT_COLS/INIT_ROWS).
    const INIT: (u16, u16) = (80, 24);

    fn shell_tool(tmpl: &str) -> Tool {
        Tool {
            cmd: "tt".into(),
            prefix: "shell".into(),
            tmpl: tmpl.into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
        }
    }

    // `label` keeps each test's config/session dir distinct (tests run in parallel
    // in one process, so a shared dir would race on the manifest / session.key).
    fn app_with(tool: &Tool, label: &str) -> (AppState, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!("ccr-attach-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
        );
        (state, tmp)
    }

    // Read the next AttachOut within a generous timeout (PTY startup is async).
    async fn next_out(rx: &mut mpsc::Receiver<AttachOut>) -> AttachOut {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("attach output within 5s")
            .expect("channel open")
    }

    #[tokio::test]
    async fn attach_emits_snapshot_before_output_with_ris() {
        let tool = shell_tool("printf BRIDGE_MARK; sleep 5");
        let (state, tmp) = app_with(&tool, "snapshot");
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
        let sess = state.get(&name).unwrap();

        let (out_tx, mut out_rx) = mpsc::channel::<AttachOut>(64);
        let (ev_tx, ev_rx) = mpsc::channel::<ClientEvent>(64);
        let task = tokio::spawn(attach_loop(sess.clone(), out_tx, ev_rx));

        // The FIRST frame is a snapshot, RIS-prefixed; the marker shows up by then
        // or in a following Output frame. Scan a few frames for the marker.
        let first = next_out(&mut out_rx).await;
        let snap = match first {
            AttachOut::Snapshot(b) => b,
            other => panic!("first frame must be a Snapshot, got {other:?}"),
        };
        assert!(snap.starts_with(b"\x1bc"), "snapshot is RIS-prefixed");

        let mut saw_marker = String::from_utf8_lossy(&snap).contains("BRIDGE_MARK");
        for _ in 0..10 {
            if saw_marker {
                break;
            }
            if let AttachOut::Output(b) | AttachOut::Snapshot(b) = next_out(&mut out_rx).await {
                if String::from_utf8_lossy(&b).contains("BRIDGE_MARK") {
                    saw_marker = true;
                }
            }
        }
        assert!(saw_marker, "the PTY's printed marker reaches the client through the loop");

        drop(ev_tx); // detach
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn two_clients_pin_pty_to_min_then_grow_back_through_loop() {
        let tool = shell_tool("sleep 5; echo two");
        let (state, tmp) = app_with(&tool, "minsize");
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
        let sess = state.get(&name).unwrap();
        assert_eq!(sess.current_size(), INIT);

        // Two attached clients, each its own attach_loop (== two register_client).
        let (a_out, _a_out_rx) = mpsc::channel::<AttachOut>(64);
        let (a_ev, a_ev_rx) = mpsc::channel::<ClientEvent>(64);
        let a = tokio::spawn(attach_loop(sess.clone(), a_out, a_ev_rx));
        let (b_out, _b_out_rx) = mpsc::channel::<AttachOut>(64);
        let (b_ev, b_ev_rx) = mpsc::channel::<ClientEvent>(64);
        let b = tokio::spawn(attach_loop(sess.clone(), b_out, b_ev_rx));

        // Wide client then a narrower one → PTY pins to the per-axis min.
        a_ev.send(ClientEvent::Resize(100, 40)).await.unwrap();
        wait_size(&sess, (100, 40)).await;
        b_ev.send(ClientEvent::Resize(60, 30)).await.unwrap();
        wait_size(&sess, (60, 30)).await;

        // The wide client growing can't widen past the narrow one.
        a_ev.send(ClientEvent::Resize(120, 50)).await.unwrap();
        wait_size(&sess, (60, 30)).await;

        // The narrow client detaches → PTY grows back for the one that remains.
        drop(b_ev);
        let _ = tokio::time::timeout(Duration::from_secs(2), b).await;
        wait_size(&sess, (120, 50)).await;

        drop(a_ev);
        let _ = tokio::time::timeout(Duration::from_secs(2), a).await;
        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Poll until the PTY reaches the expected size (resizes propagate via the
    // channel asynchronously), failing the test if it never does.
    async fn wait_size(sess: &Arc<Session>, want: (u16, u16)) {
        for _ in 0..100 {
            if sess.current_size() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(sess.current_size(), want, "PTY never reached {want:?}");
    }

    #[tokio::test]
    async fn child_exit_emits_closed() {
        // Attach to a LIVE session, then kill it — the loop must surface Closed.
        let tool = shell_tool("printf READY; sleep 5");
        let (state, tmp) = app_with(&tool, "closed");
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
        let sess = state.get(&name).unwrap();

        let (out_tx, mut out_rx) = mpsc::channel::<AttachOut>(64);
        let (_ev_tx, ev_rx) = mpsc::channel::<ClientEvent>(64);
        let task = tokio::spawn(attach_loop(sess.clone(), out_tx, ev_rx));

        // Drain the initial snapshot, then kill the child.
        assert!(matches!(next_out(&mut out_rx).await, AttachOut::Snapshot(_)));
        sess.kill();

        let mut saw_closed = false;
        for _ in 0..20 {
            if let AttachOut::Closed = next_out(&mut out_rx).await {
                saw_closed = true;
                break;
            }
        }
        assert!(saw_closed, "child exit surfaces AttachOut::Closed");
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
