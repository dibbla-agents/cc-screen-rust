// Clipboard image relay — the phone-screenshot-into-Claude path. The UI POSTs a
// PNG to /api/clip?session=<name>; we stage it (per-session slot, TTL) and write
// the paste key (Ctrl-V = 0x16) to that session's PTY. Claude Code then reads its
// clipboard via the cc-screen clipboard shim (`scripts/clip-shim.sh`, shipped by
// this agent's installer as xclip/wl-paste/pbpaste). The shim reads the staged
// image from, in order: a per-session local FILE (`$CCWEB_CLIP_FILE`, the only
// path that works when a hub-only agent has no HTTP bind) → `$CCWEB_CLIP_URL`
// (/api/clip/image.png on the agent's bind) → the Go server → the Mac clip-server.
// Both the file and the in-memory slot are written on stage (proposal 0007). We
// must NOT clear on first read — one paste triggers several probes (list-types,
// then the image) — so expiry is purely time-based.
//
// Slots are keyed by SESSION so a staged image is only served back to the session
// it was staged for (not the last-stager to any session). The shim can scope its
// fetch with `?session=` (the spawned PTY carries `CCWEB_SESSION`); when it omits
// it we fall back to the single fresh slot — the common one-paste-at-a-time case —
// and serve nothing when more than one is staged (fail safe, no cross-session
// disclosure).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

// ── local file drop (so the shim works with no HTTP bind) ────────────────────
//
// A `--hub-only` agent binds no local port: a pasted image reaches its ClipStore
// over the uplink (bulk relay), but the LOCAL shim can't curl it back. So on
// every stage we also drop the PNG into a private per-session file that the shim
// reads directly — both run on the agent host. Exported per session as
// `CCWEB_CLIP_FILE` (engine.rs). Freshness is gated by the shim (file mtime) and
// by pruning here, mirroring the ClipStore TTL.

/// Sanitize a session name into one safe filename component.
fn safe_session(session: &str) -> String {
    session
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Per-user runtime base for the drop dir: `$XDG_RUNTIME_DIR` (a 0700 tmpfs),
/// falling back to the temp dir.
fn runtime_base() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
}

fn clip_file_in(base: &Path, session: &str) -> Option<PathBuf> {
    let safe = safe_session(session);
    if safe.is_empty() {
        return None;
    }
    Some(base.join("cc-screen").join("clip").join(format!("{safe}.png")))
}

/// The per-session PNG path the local shim reads (`CCWEB_CLIP_FILE`).
pub fn session_clip_file(session: &str) -> Option<PathBuf> {
    clip_file_in(&runtime_base(), session)
}

/// Remove drop files older than the TTL, so a previous paste isn't served as a
/// stale "current image" and the dir doesn't grow unbounded.
fn prune_stale_clip_files(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("png") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|e| e > TTL).unwrap_or(false))
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Write `png` into the session's drop file (0600), pruning stale siblings first.
/// Returns the path on success. Best-effort: on any failure the shim just falls
/// back to the HTTP/Go/Mac chain.
fn write_clip_file_at(base: &Path, session: &str, png: &[u8]) -> Option<PathBuf> {
    let path = clip_file_in(base, session)?;
    let dir = path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    prune_stale_clip_files(dir);
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).ok()?;
    f.write_all(png).ok()?;
    Some(path)
}

fn write_clip_file(session: &str, png: &[u8]) {
    let _ = write_clip_file_at(&runtime_base(), session, png);
}

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
    // Also drop it to a local file so a hub-only agent's shim (no HTTP bind) can
    // read it; harmless duplicate for a bound agent.
    write_clip_file(&q.session, &body);
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

    #[test]
    fn clip_file_path_is_sanitized_and_scoped() {
        let base = Path::new("/run/user/1000");
        let p = clip_file_in(base, "claude-foo").unwrap();
        assert_eq!(p, Path::new("/run/user/1000/cc-screen/clip/claude-foo.png"));
        // A session can't escape the dir or smuggle separators.
        let p2 = clip_file_in(base, "../../etc/passwd").unwrap();
        assert_eq!(p2, Path::new("/run/user/1000/cc-screen/clip/______etc_passwd.png"));
        assert!(clip_file_in(base, "").is_none());
    }

    #[test]
    fn write_clip_file_drops_a_private_png() {
        let base = std::env::temp_dir().join(format!("ccr-clipdrop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let path = write_clip_file_at(&base, "claude-t", &[1, 2, 3, 4]).expect("write");
        assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3, 4]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "drop file must be private");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
