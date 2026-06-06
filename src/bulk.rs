//! Agent side of the dedicated bulk-transfer WebSocket.
//!
//! Big transfers (file download incl. `Range`, multipart upload up to 500 MiB,
//! clipboard-image paste) don't fit the control WS — a 500 MiB upload would
//! head-of-line-block every terminal stream. So when the hub gets such a client
//! request it sends `OpenBulk` on the control channel; the agent dials a fresh,
//! short-lived WS here, **replays the request against its REAL file-transfer
//! router** (so Range / multipart / `$HOME` confinement behave exactly as a direct
//! connection — true parity), and streams the response back.
//!
//! Bulk WS wire: hub→agent binary frames = request-body chunks, a `BULK_BODY_END`
//! text frame ends them; agent→hub a `BulkRespHead` text frame (status+headers)
//! then binary response-body chunks, then Close.

use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use cc_screen_protocol::hub::{BulkRespHead, BulkSpec, BULK_BODY_END};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderName, HeaderValue, Request};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::HeaderValue as WsHeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

use crate::engine::AppState;

const UPLOAD_MAX: usize = 500 << 20; // 500 MiB
const CLIP_MAX: usize = 25 << 20; // 25 MiB

/// The file-transfer routes only, reusing the SAME handlers the main router
/// serves — but with NO auth middleware (the hub already gated the client) and no
/// static fallback. A relayed bulk request is run through this.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/download", get(crate::files::download))
        .route("/api/upload/check", post(crate::upload::upload_check))
        .route("/api/upload", post(crate::upload::upload).layer(DefaultBodyLimit::max(UPLOAD_MAX)))
        .route("/api/clip", post(crate::clip::clip_put).layer(DefaultBodyLimit::max(CLIP_MAX)))
        .route("/api/clip/targets", get(crate::clip::clip_targets))
        .route("/api/clip/image.png", get(crate::clip::clip_image))
        .with_state(state)
}

fn bulk_ws_url(hub_base: &str, bulk_id: &str, machine_id: &str) -> String {
    let base = hub_base.trim_end_matches('/');
    let ws = if let Some(r) = base.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        base.to_string()
    };
    format!("{ws}/agent/bulk?id={}&machine={}", url_encode(bulk_id), url_encode(machine_id))
}

/// Minimal percent-encoding for the query values we send (nonce is URL-safe
/// base64; machine ids are usually hostnames, but encode defensively).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// Handle one `OpenBulk`: dial the hub's bulk WS, rebuild + run the request, and
/// stream the response back. Best-effort — a failure just means that one transfer
/// errors out on the client; the agent keeps running.
pub async fn serve(state: AppState, hub_base: String, token: Option<String>, bulk_id: String, spec: BulkSpec) {
    if let Err(e) = run(state, &hub_base, token.as_deref(), &bulk_id, spec).await {
        tracing::warn!("bulk {bulk_id}: {e}");
    }
}

async fn run(
    state: AppState,
    hub_base: &str,
    token: Option<&str>,
    bulk_id: &str,
    spec: BulkSpec,
) -> anyhow::Result<()> {
    let url = bulk_ws_url(hub_base, bulk_id, &state.inner.machine_id);
    let mut creq = url.into_client_request()?;
    if let Some(t) = token {
        let mut v = WsHeaderValue::from_str(&format!("Bearer {t}"))?;
        v.set_sensitive(true);
        creq.headers_mut().insert(AUTHORIZATION, v);
    }
    let (ws, _resp) = tokio_tungstenite::connect_async(creq).await?;
    let (mut wtx, mut wrx) = ws.split();

    // Request body: stream incoming hub→agent binary frames until BULK_BODY_END.
    let (body_tx, body_rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(16);
    tokio::spawn(async move {
        while let Some(Ok(msg)) = wrx.next().await {
            match msg {
                Message::Binary(b) => {
                    if body_tx.send(Ok(b.into())).await.is_err() {
                        break;
                    }
                }
                Message::Text(t) if t == BULK_BODY_END => break,
                Message::Close(_) => break,
                _ => {}
            }
        }
        // Dropping body_tx ends the request-body stream (EOF for the handler).
    });
    let body_stream = futures_util::stream::unfold(body_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let body = axum::body::Body::from_stream(body_stream);

    // Rebuild the HTTP request and run it through the real handlers.
    let mut builder = Request::builder().method(spec.method.as_str()).uri(spec.uri.as_str());
    for (k, v) in &spec.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            builder = builder.header(name, val);
        }
    }
    let request = builder.body(body)?;
    let response = router(state).oneshot(request).await?;

    // Send the response head, then stream the body back as binary frames.
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter_map(|(n, v)| v.to_str().ok().map(|s| (n.as_str().to_string(), s.to_string())))
        .collect();
    let head = serde_json::to_string(&BulkRespHead { status, headers })?;
    wtx.send(Message::Text(head)).await?;

    let mut data = response.into_body().into_data_stream();
    while let Some(chunk) = data.next().await {
        match chunk {
            Ok(bytes) => {
                if wtx.send(Message::Binary(bytes.to_vec())).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("bulk {bulk_id}: response body error: {e}");
                break;
            }
        }
    }
    let _ = wtx.send(Message::Close(None)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_ws_url_swaps_scheme_and_appends_id_and_machine() {
        assert_eq!(bulk_ws_url("http://hub:8840", "abc", "pine"), "ws://hub:8840/agent/bulk?id=abc&machine=pine");
        assert_eq!(bulk_ws_url("https://hub.ts.net", "n3", "box"), "wss://hub.ts.net/agent/bulk?id=n3&machine=box");
        assert_eq!(bulk_ws_url("http://hub:8840/", "x", "m"), "ws://hub:8840/agent/bulk?id=x&machine=m");
    }

    #[test]
    fn url_encode_escapes_reserved_chars() {
        assert_eq!(url_encode("pine-1.box_~"), "pine-1.box_~");
        assert_eq!(url_encode("a b/c"), "a%20b%2Fc");
    }
}
