// Clipboard image relay — the phone-screenshot-into-Claude path. The UI POSTs a
// PNG to /api/clip?session=<name>; we stage it (per-session slot, TTL) and write
// the paste key (Ctrl-V = 0x16) to that session's PTY. Claude Code then reads its
// clipboard via the cc-screen clipboard shim (`scripts/clip-shim.sh`, shipped by
// this agent's installer as xclip/wl-paste/pbpaste), which fetches
// /api/clip/image.png from `$CCWEB_CLIP_URL` — this very agent's loopback,
// exported per session in engine.rs (proposal 0007). We must NOT clear on first
// read — one paste triggers several probes (list-types, then the image) — so
// expiry is purely time-based.
//
// Slots are keyed by SESSION so a staged image is only served back to the session
// it was staged for (not the last-stager to any session). The shim can scope its
// fetch with `?session=` (the spawned PTY carries `CCWEB_SESSION`); when it omits
// it we fall back to the single fresh slot — the common one-paste-at-a-time case —
// and serve nothing when more than one is staged (fail safe, no cross-session
// disclosure).

use std::collections::HashMap;
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
    slots: Mutex<HashMap<String, (Vec<u8>, Instant)>>,
}

impl ClipStore {
    pub fn put(&self, session: &str, png: Vec<u8>) {
        let mut g = self.slots.lock().unwrap();
        g.retain(|_, (_, at)| at.elapsed() <= TTL);
        g.insert(session.to_string(), (png, Instant::now()));
    }

    /// The staged image for `session` (or, when `session` is `None`, the single
    /// fresh slot — ambiguous if more than one is staged → `None`). Stale slots
    /// are pruned on access.
    pub fn current(&self, session: Option<&str>) -> Option<Vec<u8>> {
        let mut g = self.slots.lock().unwrap();
        g.retain(|_, (_, at)| at.elapsed() <= TTL);
        match session {
            Some(s) => g.get(s).map(|(png, _)| png.clone()),
            None => match g.len() {
                1 => g.values().next().map(|(png, _)| png.clone()),
                _ => None,
            },
        }
    }
}

#[derive(Deserialize)]
pub struct ClipQuery {
    session: String,
}

/// Optional `?session=` for the shim's read probes (back-compat: absent = the
/// single fresh slot).
#[derive(Deserialize, Default)]
pub struct ClipReadQuery {
    #[serde(default)]
    session: Option<String>,
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
    app.inner.clip.put(&q.session, body.to_vec());
    sess.write_input(&[PASTE_BYTE]);
    StatusCode::NO_CONTENT.into_response()
}

// GET /api/clip/targets — the shim's "what's available" probe.
pub async fn clip_targets(State(app): State<AppState>, Query(q): Query<ClipReadQuery>) -> Response {
    let body = if app.inner.clip.current(q.session.as_deref()).is_some() {
        "image/png"
    } else {
        ""
    };
    ([(axum::http::header::CONTENT_TYPE, "text/plain")], body).into_response()
}

// GET /api/clip/image.png — serve the staged PNG (idempotent within the TTL).
pub async fn clip_image(State(app): State<AppState>, Query(q): Query<ClipReadQuery>) -> Response {
    match app.inner.clip.current(q.session.as_deref()) {
        Some(png) => ([(axum::http::header::CONTENT_TYPE, "image/png")], png).into_response(),
        None => (StatusCode::NOT_FOUND, "no image").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_scoped_by_session() {
        let s = ClipStore::default();
        s.put("claude-a", vec![1, 2, 3]);
        s.put("claude-b", vec![4, 5, 6]);
        // Each session sees only its own image.
        assert_eq!(s.current(Some("claude-a")), Some(vec![1, 2, 3]));
        assert_eq!(s.current(Some("claude-b")), Some(vec![4, 5, 6]));
        assert_eq!(s.current(Some("claude-c")), None);
        // No session + more than one staged → ambiguous → nothing (no leak).
        assert_eq!(s.current(None), None);
    }

    #[test]
    fn no_session_serves_the_single_fresh_slot() {
        let s = ClipStore::default();
        s.put("only", vec![9]);
        assert_eq!(s.current(None), Some(vec![9]), "single staged image is unambiguous");
    }
}
