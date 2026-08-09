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
//
// Delivery is assistant-aware (proposal 0066), dispatched on the session's
// server-owned `image_paste` strategy — never on client input:
//   - ClipboardProbe (Claude & default): stage in the ClipStore + drop file,
//     send Ctrl-V; the clipboard shim serves the image (proposal 0007).
//   - BracketedImagePath (Codex): stage a unique durable private PNG and
//     bracketed-paste its shell-escaped absolute path — no Enter, no Ctrl-V.
//     Codex attaches a recognized pasted image path itself; its native
//     clipboard read needs X11/Wayland a headless box doesn't have.
//
// A 204 means the bytes were staged and the PTY write completed — not that the
// assistant has parsed the attachment.
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
    match sess.image_paste {
        crate::tools::ImagePasteStrategy::ClipboardProbe => {
            app.inner.clip.put(&q.session, body.to_vec());
            // Also drop it to a local file so a hub-only agent's shim (no HTTP
            // bind) can read it; harmless duplicate for a bound agent.
            write_clip_file(&q.session, &body);
            let (written, err) = sess.write_input_checked(&[PASTE_BYTE]);
            if written == 0 && err.is_some() {
                return (StatusCode::SERVICE_UNAVAILABLE, "session input closed").into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        crate::tools::ImagePasteStrategy::BracketedImagePath => {
            use crate::clip_attachment::StageError;
            let path = match app.inner.attachments.stage(&q.session, &body) {
                Ok(p) => p,
                Err(StageError::InvalidPng) => {
                    return (StatusCode::UNPROCESSABLE_ENTITY, "not a valid PNG image")
                        .into_response();
                }
                Err(StageError::Quota) => {
                    return (
                        StatusCode::INSUFFICIENT_STORAGE,
                        "image attachment quota exhausted for this session",
                    )
                        .into_response();
                }
                Err(StageError::Unstageable(why)) => {
                    tracing::warn!("clip: could not stage attachment for {}: {why}", q.session);
                    return (StatusCode::INTERNAL_SERVER_ERROR, "could not stage image")
                        .into_response();
                }
            };
            let Some(escaped) = crate::clip_attachment::escaped_path(&path) else {
                app.inner.attachments.discard(&q.session, &path, body.len());
                return (StatusCode::INTERNAL_SERVER_ERROR, "could not stage image")
                    .into_response();
            };
            // Exactly ESC[200~<escaped-path>ESC[201~ — no trailing Enter, so
            // the image attaches to the composer without submitting.
            let buf = cc_screen_protocol::wrap_bracketed_paste(&escaped, false);
            let (written, err) = sess.write_input_checked(&buf);
            if err.is_some() || written < buf.len() {
                if written == 0 {
                    // Proven zero-byte failure: the assistant can't have seen
                    // the path, so the file and its quota roll back.
                    app.inner.attachments.discard(&q.session, &path, body.len());
                }
                // Partial/ambiguous delivery keeps the file until normal
                // session cleanup — Codex may already hold the path.
                return (StatusCode::SERVICE_UNAVAILABLE, "session input failed").into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
    }
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

    /// A minimal valid-header PNG (signature + IHDR), padded.
    fn tiny_png() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&2u32.to_be_bytes());
        v.extend_from_slice(&2u32.to_be_bytes());
        v.resize(64, 0);
        v
    }

    #[cfg(unix)]
    fn dispatch_tool(prefix: &str, cmd: &str, tmpl: &str, strat: crate::tools::ImagePasteStrategy) -> crate::tools::Tool {
        crate::tools::Tool {
            cmd: cmd.into(),
            prefix: prefix.into(),
            tmpl: tmpl.into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
            install_hint: None,
            update_cmd: None,
            image_paste: strat,
        }
    }

    /// Poll `path` until `pred(bytes)` or timeout; returns the final content.
    #[cfg(unix)]
    async fn wait_for(path: &Path, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        for _ in 0..100 {
            if let Ok(b) = std::fs::read(path) {
                if pred(&b) {
                    return b;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::read(path).unwrap_or_default()
    }

    // 0066 Part C: the strategy dispatch, end to end against a real PTY. The
    // fake assistant is `stty raw -echo; cat > <file>` — raw mode so the line
    // discipline neither buffers on newline nor eats the 0x16 lnext char, and
    // the recorded bytes are exactly what the assistant's stdin saw.
    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_delivers_bracketed_path_to_codex_and_ctrl_v_to_probe() {
        use axum::extract::{Query, State};
        let tmp = std::env::temp_dir().join(format!("ccr-clipdisp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let seen_codex = tmp.join("seen-codex.bin");
        let seen_probe = tmp.join("seen-probe.bin");
        let codex = dispatch_tool(
            "codex",
            "coc",
            &format!("stty raw -echo; cat > '{}'", seen_codex.display()),
            crate::tools::ImagePasteStrategy::BracketedImagePath,
        );
        let probe = dispatch_tool(
            "claude",
            "cc",
            &format!("stty raw -echo; cat > '{}'", seen_probe.display()),
            crate::tools::ImagePasteStrategy::ClipboardProbe,
        );
        let app = crate::engine::AppState::new(
            vec![codex.clone(), probe.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let dir = tmp.to_string_lossy().to_string();
        let c = app.create(&codex, "t", &dir, vec![], false, true).unwrap();
        let p = app.create(&probe, "t", &dir, vec![], false, true).unwrap();
        // Let the fake assistants reach raw mode before any paste bytes land.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Unknown session → 404, nothing staged.
        let r = clip_put(
            State(app.clone()),
            Query(ClipQuery { session: "codex-nope".into() }),
            Bytes::from(tiny_png()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);

        // Invalid PNG to the codex session → 422, no file, no input.
        let r = clip_put(
            State(app.clone()),
            Query(ClipQuery { session: c.clone() }),
            Bytes::from_static(b"not a png"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Two rapid valid pastes → 204 + two distinct durable paths delivered
        // as exact bracketed-paste frames, no Enter, no Ctrl-V.
        for _ in 0..2 {
            let r = clip_put(
                State(app.clone()),
                Query(ClipQuery { session: c.clone() }),
                Bytes::from(tiny_png()),
            )
            .await;
            assert_eq!(r.status(), StatusCode::NO_CONTENT);
        }
        let att_dir = tmp.join("clip-attachments").join(&c);
        let mut staged: Vec<PathBuf> =
            std::fs::read_dir(&att_dir).unwrap().flatten().map(|e| e.path()).collect();
        staged.sort();
        assert_eq!(staged.len(), 2, "each paste staged its own file");
        assert_ne!(staged[0], staged[1]);
        for f in &staged {
            assert_eq!(std::fs::read(f).unwrap(), tiny_png(), "exact bytes retained");
        }
        let seen = wait_for(&seen_codex, |b| b.windows(6).filter(|w| w == b"\x1b[201~").count() >= 2).await;
        let text = String::from_utf8_lossy(&seen);
        for f in &staged {
            let esc = crate::clip_attachment::escaped_path(f).unwrap();
            assert!(
                text.contains(&format!("\x1b[200~{esc}\x1b[201~")),
                "exact bracketed frame for {f:?} in {text:?}"
            );
        }
        assert!(!seen.contains(&0x16), "no Ctrl-V on the bracketed path");
        assert!(!seen.contains(&b'\r'), "no Enter — the image must not submit");

        // The probe session still gets the 0007 contract: 0x16 + staged slot.
        let r = clip_put(
            State(app.clone()),
            Query(ClipQuery { session: p.clone() }),
            Bytes::from(tiny_png()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let seen = wait_for(&seen_probe, |b| b.contains(&0x16)).await;
        assert_eq!(seen, vec![0x16], "exactly the paste key, nothing else");
        assert_eq!(app.inner.clip.current(Some(&p)), Some(tiny_png()), "shim slot staged");

        // Cleanup: kill both (hard exit keeps attachments — resume may need
        // them); then a purge on the codex session removes its directory.
        app.inner.attachments.purge_session(&c);
        assert!(!att_dir.exists());
        app.get(&c).map(|s| s.kill());
        app.get(&p).map(|s| s.kill());
        let _ = std::fs::remove_dir_all(&tmp);
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
