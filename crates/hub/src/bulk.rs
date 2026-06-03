//! Hub side of bulk transfers (download / upload / clipboard image).
//!
//! These don't fit the control WS (a 500 MiB upload would head-of-line-block
//! every terminal). So a client request to a bulk endpoint is relayed over a
//! dedicated, short-lived WS: the hub allocates an id, tells the owning agent to
//! dial `/agent/bulk?id=` (via `OpenBulk` on the control channel), then bridges
//! the client request body → agent and the agent response → client, streaming
//! both ways. The agent replays the request against its REAL handlers, so Range /
//! multipart / confinement all behave exactly as a direct connection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use cc_screen_protocol::hub::{encode_frame, BulkRespHead, BulkSpec, HubMsg, BULK_BODY_END};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::state::HubState;

/// In-flight bulk transfers, keyed by a hub-allocated id the agent dials back with.
#[derive(Clone, Default)]
pub struct BulkRegistry {
    slots: Arc<Mutex<HashMap<u32, BulkSlot>>>,
    next: Arc<AtomicU32>,
}

struct BulkSlot {
    /// Client request body → agent (drained by the `/agent/bulk` bridge).
    req_body: mpsc::Receiver<Vec<u8>>,
    /// Agent response head → the waiting client handler.
    head_tx: oneshot::Sender<BulkRespHead>,
    /// Agent response body → the client handler's response stream.
    body_tx: mpsc::Sender<Vec<u8>>,
}

impl BulkRegistry {
    fn alloc(&self) -> u32 {
        self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }
    fn take(&self, id: u32) -> Option<BulkSlot> {
        self.slots.lock().unwrap().remove(&id)
    }
}

/// Per-RFC hop-by-hop headers (plus Host) that must not be relayed.
fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "connection" | "keep-alive" | "proxy-authenticate" | "proxy-authorization"
            | "te" | "trailer" | "transfer-encoding" | "upgrade" | "content-length"
    )
}

fn qparam(query: &str, key: &str) -> Option<String> {
    query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| {
        // minimal percent-decode for the few chars we care about (spaces/slashes)
        v.replace("%2F", "/").replace("%20", " ").to_string()
    })
}

/// The client-facing handler for every bulk route (download/upload/clip/…). It
/// relays the whole HTTP request to the owning agent and streams the response.
pub async fn proxy(State(hub): State<HubState>, req: Request) -> Response {
    let method = req.method().as_str().to_string();
    let uri = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    // Resolve the machine: explicit ?machine=, else by ?session= owner, else the
    // single online machine (the PWA sends neither for download/upload).
    let query = req.uri().query().unwrap_or("").to_string();
    let machine = qparam(&query, "machine").unwrap_or_default();
    let session = qparam(&query, "session");
    let Some(agent) = hub.registry.resolve(&machine, session.as_deref()) else {
        return (StatusCode::NOT_FOUND, "no online machine for that request").into_response();
    };
    let headers: Vec<(String, String)> = req
        .headers()
        .iter()
        .filter(|(n, _)| !is_hop_by_hop(n.as_str()))
        .filter_map(|(n, v)| v.to_str().ok().map(|s| (n.as_str().to_string(), s.to_string())))
        .collect();
    let client_body = req.into_body();

    // Register the transfer + tell the agent to dial back.
    let id = hub.bulk.alloc();
    let (req_tx, req_rx) = mpsc::channel::<Vec<u8>>(16);
    let (head_tx, head_rx) = oneshot::channel::<BulkRespHead>();
    let (body_tx, body_rx) = mpsc::channel::<Vec<u8>>(16);
    hub.bulk.slots.lock().unwrap().insert(id, BulkSlot { req_body: req_rx, head_tx, body_tx });

    let frame = encode_frame(&HubMsg::OpenBulk { req: id, bulk: BulkSpec { method, uri, headers } }, b"");
    if agent.to_agent().send(frame).await.is_err() {
        hub.bulk.take(id);
        return (StatusCode::SERVICE_UNAVAILABLE, "machine offline").into_response();
    }

    // Pump the client's request body toward the agent (drop ends it).
    tokio::spawn(async move {
        let mut data = client_body.into_data_stream();
        while let Some(chunk) = data.next().await {
            match chunk {
                Ok(b) => {
                    if req_tx.send(b.to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Wait for the agent's response head, then stream its body to the client.
    let head = match tokio::time::timeout(Duration::from_secs(30), head_rx).await {
        Ok(Ok(h)) => h,
        _ => {
            hub.bulk.take(id);
            return (StatusCode::GATEWAY_TIMEOUT, "agent did not respond").into_response();
        }
    };
    let mut builder = Response::builder().status(StatusCode::from_u16(head.status).unwrap_or(StatusCode::OK));
    for (k, v) in &head.headers {
        if let (Ok(name), Ok(val)) =
            (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v))
        {
            builder = builder.header(name, val);
        }
    }
    let body_stream = futures_util::stream::unfold(body_rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(axum::body::Bytes::from(b)), rx))
    });
    builder.body(Body::from_stream(body_stream)).unwrap_or_else(|_| {
        (StatusCode::INTERNAL_SERVER_ERROR, "bad response").into_response()
    })
}

#[derive(Deserialize)]
pub struct BulkIdQuery {
    id: u32,
}

/// `GET /agent/bulk?id=` — the agent dials this to run one transfer. Token-gated
/// like `/agent/ws`; matched to the waiting client transfer by `id`.
pub async fn agent_bulk(
    State(hub): State<HubState>,
    headers: HeaderMap,
    Query(q): Query<BulkIdQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);
    if !hub.handshake_token_plausible(token) {
        return (StatusCode::UNAUTHORIZED, "bad agent token").into_response();
    }
    let Some(slot) = hub.bulk.take(q.id) else {
        return (StatusCode::NOT_FOUND, "unknown or expired bulk id").into_response();
    };
    ws.on_upgrade(move |socket| bridge(socket, slot))
}

async fn bridge(socket: WebSocket, slot: BulkSlot) {
    let (mut tx, mut rx) = socket.split();
    let BulkSlot { mut req_body, head_tx, body_tx } = slot;

    // Forward the client request body to the agent, then the end marker.
    tokio::spawn(async move {
        while let Some(chunk) = req_body.recv().await {
            if tx.send(Message::Binary(chunk)).await.is_err() {
                return;
            }
        }
        let _ = tx.send(Message::Text(BULK_BODY_END.to_string())).await;
        // tx drops here; the read half stays open for the agent's response.
    });

    // Read the agent's response: first a head (text), then body (binary).
    let mut head_tx = Some(head_tx);
    while let Some(Ok(msg)) = rx.next().await {
        match msg {
            Message::Text(t) => {
                if let (Some(htx), Ok(h)) = (head_tx.take(), serde_json::from_str::<BulkRespHead>(&t)) {
                    let _ = htx.send(h);
                }
            }
            Message::Binary(b) => {
                if body_tx.send(b).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    // Dropping body_tx ends the client's response body stream.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qparam_extracts_and_decodes() {
        let q = "path=%2Fhome%2Fu%2Ff&machine=pine&session=claude-x";
        assert_eq!(qparam(q, "machine").as_deref(), Some("pine"));
        assert_eq!(qparam(q, "session").as_deref(), Some("claude-x"));
        assert_eq!(qparam(q, "path").as_deref(), Some("/home/u/f"));
        assert_eq!(qparam(q, "absent"), None);
    }

    #[test]
    fn hop_by_hop_filters_the_right_headers() {
        for h in ["Connection", "content-length", "Transfer-Encoding", "host"] {
            assert!(is_hop_by_hop(h), "{h} should be hop-by-hop");
        }
        for h in ["range", "content-type", "content-disposition", "accept-ranges"] {
            assert!(!is_hop_by_hop(h), "{h} must be relayed");
        }
    }
}
