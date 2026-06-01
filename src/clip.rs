// Clipboard image relay — the phone-screenshot-into-Claude path. The UI POSTs a
// PNG to /api/clip; we stage it (single slot, TTL) and write the paste key
// (Ctrl-V = 0x16) to the session's PTY. Claude Code then reads its clipboard via
// the cc-screen clipboard shim, which (web-aware) fetches /api/clip/image.png.
// We must NOT clear on first read — one paste triggers several probes
// (list-types, then the image) — so expiry is purely time-based.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::engine::AppState;

const TTL: Duration = Duration::from_secs(20);
pub const PASTE_BYTE: u8 = 0x16; // Ctrl-V — the key Claude Code reads the clipboard on

#[derive(Default)]
pub struct ClipStore {
    slot: Mutex<Option<(Vec<u8>, Instant)>>,
}

impl ClipStore {
    pub fn put(&self, png: Vec<u8>) {
        *self.slot.lock().unwrap() = Some((png, Instant::now()));
    }

    /// The staged image if still fresh, else None (dropping stale bytes).
    pub fn current(&self) -> Option<Vec<u8>> {
        let mut g = self.slot.lock().unwrap();
        if let Some((png, at)) = g.as_ref() {
            if at.elapsed() <= TTL {
                return Some(png.clone());
            }
        }
        *g = None;
        None
    }
}

#[derive(Deserialize)]
pub struct ClipQuery {
    session: String,
}

// POST /api/clip?session=<name> — body is a PNG. Body size is bounded by the
// DefaultBodyLimit layer on this route (see main.rs).
pub async fn clip_put(
    State(app): State<AppState>,
    Query(q): Query<ClipQuery>,
    body: Bytes,
) -> Response {
    let Some(sess) = app.get(&q.session) else {
        return (StatusCode::NOT_FOUND, "unknown session").into_response();
    };
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty image").into_response();
    }
    app.inner.clip.put(body.to_vec());
    sess.write_input(&[PASTE_BYTE]);
    StatusCode::NO_CONTENT.into_response()
}

// GET /api/clip/targets — the shim's "what's available" probe.
pub async fn clip_targets(State(app): State<AppState>) -> Response {
    let body = if app.inner.clip.current().is_some() {
        "image/png"
    } else {
        ""
    };
    ([(axum::http::header::CONTENT_TYPE, "text/plain")], body).into_response()
}

// GET /api/clip/image.png — serve the staged PNG (idempotent within the TTL).
pub async fn clip_image(State(app): State<AppState>) -> Response {
    match app.inner.clip.current() {
        Some(png) => ([(axum::http::header::CONTENT_TYPE, "image/png")], png).into_response(),
        None => (StatusCode::NOT_FOUND, "no image").into_response(),
    }
}
