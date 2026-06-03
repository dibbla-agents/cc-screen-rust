//! The per-session WebSocket task: connect, pump server→client byte frames into
//! the app, and forward client→server input/resize. Reconnects with backoff on
//! a dropped connection; the app auto-detaches when the *session* disappears
//! from the poll, so a truly-dead session doesn't reconnect forever.

use std::time::Duration;

use cc_screen_protocol::WsClientFrame;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::app::{AppMsg, PaneMsg};
use crate::pane::{ConnState, WsOut};

pub async fn run(
    url: String,
    token: Option<String>,
    id: u64,
    init_cols: u16,
    init_rows: u16,
    mut out_rx: mpsc::Receiver<WsOut>,
    app_tx: mpsc::Sender<AppMsg>,
) {
    let state = |s: ConnState| AppMsg::Pane { id, msg: PaneMsg::State(s) };
    let mut cols = init_cols.max(1);
    let mut rows = init_rows.max(1);
    let mut backoff_ms = 500u64;

    loop {
        let _ = app_tx.send(state(ConnState::Connecting)).await;

        // The browser authenticates the WS handshake with its session cookie;
        // the TUI has no cookie jar, so it sends the API token on the handshake
        // (rebuilt each attempt — the Request isn't reusable across reconnects).
        let req = match client_request(&url, token.as_deref()) {
            Ok(r) => r,
            Err(_) => {
                let _ = app_tx.send(state(ConnState::Closed)).await;
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(5000);
                continue;
            }
        };
        let stream = match tokio_tungstenite::connect_async(req).await {
            Ok((s, _resp)) => s,
            Err(_) => {
                let _ = app_tx.send(state(ConnState::Closed)).await;
                sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(5000);
                continue;
            }
        };
        backoff_ms = 500;
        let (mut write, mut read) = stream.split();
        let _ = app_tx.send(state(ConnState::Open)).await;
        // (Re)tell the server our current size; on reconnect the server replies
        // with a fresh RIS-prefixed snapshot, which the pane uses to repaint.
        let _ = write.send(Message::Text(resize_json(cols, rows))).await;

        let mut stop = false;
        loop {
            tokio::select! {
                incoming = read.next() => match incoming {
                    Some(Ok(Message::Binary(b))) => {
                        if app_tx.send(AppMsg::Pane { id, msg: PaneMsg::Bytes(b) }).await.is_err() {
                            stop = true;
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break, // → reconnect
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break, // → reconnect
                },
                outgoing = out_rx.recv() => match outgoing {
                    Some(WsOut::Input(bytes)) => {
                        if write.send(Message::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    Some(WsOut::Resize(c, r)) => {
                        cols = c.max(1);
                        rows = r.max(1);
                        if write.send(Message::Text(resize_json(cols, rows))).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        stop = true; // the Pane (and its out_tx) was dropped → detach
                        break;
                    }
                },
            }
        }

        if stop {
            return;
        }
        let _ = app_tx.send(state(ConnState::Closed)).await;
        sleep(Duration::from_millis(300)).await;
    }
}

fn resize_json(cols: u16, rows: u16) -> String {
    serde_json::to_string(&WsClientFrame::resize(cols, rows)).unwrap()
}

/// Build the WS handshake request for `url`, attaching `Authorization: Bearer`
/// when a token is configured (a no-op against an unauthenticated server).
fn client_request(url: &str, token: Option<&str>) -> Result<Request, anyhow::Error> {
    let mut req = url.into_client_request()?;
    if let Some(t) = token {
        let mut val = HeaderValue::from_str(&format!("Bearer {t}"))?;
        val.set_sensitive(true);
        req.headers_mut().insert(AUTHORIZATION, val);
    }
    Ok(req)
}
