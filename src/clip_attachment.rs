//! Per-session image attachments for path-paste assistants (proposal 0066).
//!
//! Codex doesn't shell out to a clipboard tool — it opens the X11/Wayland
//! clipboard natively, which a headless box doesn't have. What it *does*
//! support is an explicitly pasted, readable image path: it normalizes the
//! path, checks dimensions, and attaches the image. So for
//! `ImagePasteStrategy::BracketedImagePath` tools we stage each pasted PNG as
//! a unique private file and bracket-paste its path (see `clip.rs`).
//!
//! Unlike the [0007] compatibility file (one mutable slot, 20 s TTL — the shim
//! consumes it immediately), these files are referenced by the assistant for
//! as long as a draft or transcript mentions them: every paste gets a fresh
//! path that is never overwritten, files survive the normal agent
//! stop/update/restart/resume cycle, and cleanup happens only when the session
//! is permanently removed (delete / clean non-restart exit) or when startup GC
//! finds a directory no manifest record or live session claims.
//!
//! Storage lives in a private dir adjacent to the manifest
//! (`<config_dir>/clip-attachments/<session>/img-*.png`, 0700/0600) — a
//! persistent location, deliberately NOT `$XDG_RUNTIME_DIR`/tmp which can
//! vanish on reboot while a Codex transcript still references the path.
//!
//! These are logical lifecycle guarantees, not an OS sandbox: modes protect
//! against other OS users, not against another same-UID process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Per durable session record: max distinct attachment files.
const MAX_FILES_PER_SESSION: usize = 64;
/// Per durable session record: max total attachment bytes (256 MiB).
const MAX_BYTES_PER_SESSION: u64 = 256 * 1024 * 1024;
/// Across the whole agent: max total attachment bytes (1 GiB).
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Codex's paste-burst threshold is 1000 chars; the pasted representation
/// (shell-escaped absolute path) must stay comfortably under it.
const MAX_ESCAPED_PATH_CHARS: usize = 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum StageError {
    /// Not a plausible PNG (bad signature/IHDR, zero or absurd dimensions).
    InvalidPng,
    /// A per-session or agent-wide quota would be exceeded.
    Quota,
    /// The session name or resulting path can't be represented safely, or an
    /// I/O error occurred. Nothing was staged.
    Unstageable(String),
}

#[derive(Default)]
struct Usage {
    files: usize,
    bytes: u64,
}

struct StoreState {
    per_session: HashMap<String, Usage>,
    total_bytes: u64,
    seq: u64,
}

pub struct AttachmentStore {
    root: PathBuf,
    state: Mutex<StoreState>,
}

/// True iff `name` is safe as a single path component (the engine's session
/// names — `<prefix>-<sanitized short>` — always are; reject anything else
/// rather than lossily mapping two names onto one directory).
fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Bounded PNG plausibility check: 8-byte signature + IHDR header fields only.
/// Never decodes attacker-controlled compressed pixel data.
pub fn validate_png(bytes: &[u8]) -> bool {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    // Signature, IHDR length (13) + type, then width/height (big-endian u32).
    if bytes.len() < 33 || bytes[..8] != SIG {
        return false;
    }
    if bytes[8..12] != [0, 0, 0, 13] || &bytes[12..16] != b"IHDR" {
        return false;
    }
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (w, h) = (be(&bytes[16..20]) as u64, be(&bytes[20..24]) as u64);
    w != 0 && h != 0 && w <= 16_384 && h <= 16_384 && w.checked_mul(h).is_some_and(|p| p <= 100_000_000)
}

fn set_private_dir(_p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(_p, std::fs::Permissions::from_mode(0o700));
    }
}

impl AttachmentStore {
    /// Open the store rooted at `<config_dir>/clip-attachments`, garbage-collect
    /// directories no name in `claimed` owns (manifest records + live sessions
    /// — at startup the registry is empty, so `claimed` is the manifest), and
    /// reconstruct per-session and agent-wide usage from the surviving files —
    /// so a restart can never reset or bypass a quota.
    pub fn open(config_dir: &Path, claimed: &[String]) -> AttachmentStore {
        let root = config_dir.join("clip-attachments");
        let _ = std::fs::create_dir_all(&root);
        set_private_dir(&root);
        let mut state = StoreState { per_session: HashMap::new(), total_bytes: 0, seq: 0 };
        if let Ok(rd) = std::fs::read_dir(&root) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = entry.path();
                // Never follow a symlink; a non-dir or unclaimed dir is orphaned.
                let is_real_dir = entry
                    .path()
                    .symlink_metadata()
                    .map(|m| m.file_type().is_dir())
                    .unwrap_or(false);
                if !is_real_dir || !safe_component(&name) || !claimed.iter().any(|c| c == &name) {
                    let _ = if is_real_dir {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    continue;
                }
                let mut usage = Usage::default();
                if let Ok(files) = std::fs::read_dir(&path) {
                    for f in files.flatten() {
                        match f.path().symlink_metadata() {
                            Ok(m) if m.file_type().is_file() => {
                                usage.files += 1;
                                usage.bytes += m.len();
                            }
                            // Anything else in here isn't ours — remove it.
                            _ => {
                                let _ = std::fs::remove_file(f.path());
                                let _ = std::fs::remove_dir_all(f.path());
                            }
                        }
                    }
                }
                state.total_bytes += usage.bytes;
                state.per_session.insert(name, usage);
            }
        }
        AttachmentStore { root, state: Mutex::new(state) }
    }

    /// Stage `png` as a fresh private file for `session` and return its
    /// absolute path. Transactional: quota check, exclusive create, full
    /// write, then registration — any failure leaves no partial file and no
    /// reserved quota.
    pub fn stage(&self, session: &str, png: &[u8]) -> Result<PathBuf, StageError> {
        if !safe_component(session) {
            return Err(StageError::Unstageable("unsafe session name".into()));
        }
        if !validate_png(png) {
            return Err(StageError::InvalidPng);
        }
        let bytes = png.len() as u64;
        // One lock spans reserve→create→write→register, serializing staging
        // with rollback/cleanup so usage can never exceed a bound.
        let mut st = self.state.lock().unwrap();
        {
            let usage = st.per_session.entry(session.to_string()).or_default();
            if usage.files + 1 > MAX_FILES_PER_SESSION
                || usage.bytes + bytes > MAX_BYTES_PER_SESSION
            {
                return Err(StageError::Quota);
            }
        }
        if st.total_bytes + bytes > MAX_TOTAL_BYTES {
            return Err(StageError::Quota);
        }
        let dir = self.root.join(session);
        std::fs::create_dir_all(&dir)
            .map_err(|e| StageError::Unstageable(format!("mkdir: {e}")))?;
        set_private_dir(&dir);
        // Server-generated URL-safe name; uniqueness enforced by create_new.
        let mut path = None;
        for _ in 0..8 {
            st.seq += 1;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let cand = dir.join(format!("img-{:04}-{nanos:09}.png", st.seq));
            let mut opts = std::fs::OpenOptions::new();
            // create_new = O_CREAT|O_EXCL: fails if anything (including a
            // symlink, even dangling) already sits at the path — the
            // exclusive, no-follow creation the design requires.
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&cand) {
                Ok(f) => {
                    path = Some((cand, f));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(StageError::Unstageable(format!("create: {e}"))),
            }
        }
        let Some((path, mut file)) = path else {
            return Err(StageError::Unstageable("could not allocate a unique file".into()));
        };
        // The pasted representation must survive Codex's normalization: an
        // absolute UTF-8 path with no control characters, short enough after
        // shell escaping. Checked before any bytes land.
        if let Err(why) = validate_representation(&path) {
            let _ = std::fs::remove_file(&path);
            return Err(StageError::Unstageable(why));
        }
        use std::io::Write;
        if let Err(e) = file.write_all(png).and_then(|_| file.flush()) {
            let _ = std::fs::remove_file(&path);
            return Err(StageError::Unstageable(format!("write: {e}")));
        }
        drop(file);
        let usage = st.per_session.entry(session.to_string()).or_default();
        usage.files += 1;
        usage.bytes += bytes;
        st.total_bytes += bytes;
        Ok(path)
    }

    /// Roll back one just-staged file — allowed only when the caller proved
    /// zero bytes of its path reached the PTY (see clip.rs). After a partial
    /// or ambiguous delivery the file must stay until normal session cleanup,
    /// because the assistant may already have received the path.
    pub fn discard(&self, session: &str, path: &Path, bytes: usize) {
        let mut st = self.state.lock().unwrap();
        if !path.starts_with(self.root.join(session)) {
            return;
        }
        if std::fs::remove_file(path).is_ok() {
            if let Some(usage) = st.per_session.get_mut(session) {
                usage.files = usage.files.saturating_sub(1);
                usage.bytes = usage.bytes.saturating_sub(bytes as u64);
            }
            st.total_bytes = st.total_bytes.saturating_sub(bytes as u64);
        }
    }

    /// Permanent removal of a session's attachments — the session is being
    /// deleted / has exited for good (never on a restart/resume stop).
    pub fn purge_session(&self, session: &str) {
        if !safe_component(session) {
            return;
        }
        let mut st = self.state.lock().unwrap();
        if let Some(usage) = st.per_session.remove(session) {
            st.total_bytes = st.total_bytes.saturating_sub(usage.bytes);
        }
        let _ = std::fs::remove_dir_all(self.root.join(session));
    }
}

/// The path constraints the paste representation must satisfy (0066 Part B
/// step 5): absolute, valid UTF-8, no control chars / ESC / bracketed-paste
/// terminator, and under Codex's paste threshold after shell escaping.
fn validate_representation(path: &Path) -> Result<(), String> {
    let Some(s) = path.to_str() else {
        return Err("path is not valid UTF-8".into());
    };
    if !path.is_absolute() {
        return Err("path is not absolute".into());
    }
    if s.chars().any(|c| c.is_control()) {
        return Err("path contains control characters".into());
    }
    let escaped = crate::tools::shell_quote(s);
    if escaped.chars().count() >= MAX_ESCAPED_PATH_CHARS {
        return Err("path too long to paste".into());
    }
    Ok(())
}

/// Shell-escape the staged path for the bracketed paste (POSIX single-quote /
/// Windows double-quote via the launch-shell quoting rules).
pub fn escaped_path(path: &Path) -> Option<String> {
    validate_representation(path).ok()?;
    Some(crate::tools::shell_quote(path.to_str()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal syntactically-valid PNG header (signature + IHDR) with the
    /// given dimensions, padded to `len` bytes.
    fn png(w: u32, h: u32, len: usize) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.resize(len.max(33), 0);
        v
    }

    fn tmp_store(tag: &str, claimed: &[String]) -> (PathBuf, AttachmentStore) {
        let dir = std::env::temp_dir().join(format!("ccr-att-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = AttachmentStore::open(&dir, claimed);
        (dir, store)
    }

    #[test]
    fn png_validation_bounds() {
        assert!(validate_png(&png(100, 50, 64)));
        assert!(!validate_png(b"not a png"));
        assert!(!validate_png(&png(0, 50, 64)), "zero width");
        assert!(!validate_png(&png(100, 20_000, 64)), "axis over 16384");
        assert!(!validate_png(&png(16_000, 16_000, 64)), "pixel count over 100M");
        let mut bad = png(10, 10, 64);
        bad[12..16].copy_from_slice(b"JUNK");
        assert!(!validate_png(&bad), "IHDR type must match");
    }

    #[test]
    fn stage_gives_unique_private_readable_paths() {
        let (dir, store) = tmp_store("uniq", &[]);
        let a = store.stage("codex-t", &png(10, 10, 100)).unwrap();
        let b = store.stage("codex-t", &png(10, 10, 200)).unwrap();
        assert_ne!(a, b, "every paste gets a fresh path");
        assert!(a.is_absolute());
        assert_eq!(std::fs::read(&a).unwrap().len(), 100);
        assert_eq!(std::fs::read(&b).unwrap().len(), 200);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dmode = std::fs::metadata(a.parent().unwrap()).unwrap().permissions().mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
        // The escaped representation round-trips a plain path unquoted-cleanly.
        let esc = escaped_path(&a).unwrap();
        assert!(esc.contains(a.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_invalid_and_unsafe() {
        let (dir, store) = tmp_store("bad", &[]);
        assert_eq!(store.stage("codex-t", b"nope"), Err(StageError::InvalidPng));
        assert!(matches!(
            store.stage("../escape", &png(1, 1, 40)),
            Err(StageError::Unstageable(_))
        ));
        assert!(matches!(store.stage("", &png(1, 1, 40)), Err(StageError::Unstageable(_))));
        // Nothing landed on disk for the failed stages.
        assert!(std::fs::read_dir(dir.join("clip-attachments")).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn per_session_file_quota() {
        let (dir, store) = tmp_store("quota", &[]);
        for _ in 0..MAX_FILES_PER_SESSION {
            store.stage("codex-q", &png(1, 1, 40)).unwrap();
        }
        assert_eq!(store.stage("codex-q", &png(1, 1, 40)), Err(StageError::Quota));
        // Another session is unaffected.
        assert!(store.stage("codex-other", &png(1, 1, 40)).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn discard_rolls_back_quota() {
        let (dir, store) = tmp_store("discard", &[]);
        let p = store.stage("codex-d", &png(1, 1, 40)).unwrap();
        store.discard("codex-d", &p, 40);
        assert!(!p.exists());
        // The slot is free again: refill to the cap.
        for _ in 0..MAX_FILES_PER_SESSION {
            store.stage("codex-d", &png(1, 1, 40)).unwrap();
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn purge_removes_only_that_session() {
        let (dir, store) = tmp_store("purge", &[]);
        let a = store.stage("codex-a", &png(1, 1, 40)).unwrap();
        let b = store.stage("codex-b", &png(1, 1, 40)).unwrap();
        store.purge_session("codex-a");
        assert!(!a.exists());
        assert!(b.exists(), "another session's live file is untouched");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn open_reconstructs_usage_and_gcs_orphans() {
        let (dir, store) = tmp_store("gc", &[]);
        let kept = store.stage("codex-keep", &png(1, 1, 50)).unwrap();
        let orphan = store.stage("codex-orphan", &png(1, 1, 50)).unwrap();
        drop(store);
        // Reopen claiming only codex-keep (as the manifest would).
        let store = AttachmentStore::open(&dir, &["codex-keep".into()]);
        assert!(kept.exists(), "claimed attachment survives restart");
        assert!(!orphan.exists(), "unclaimed directory is collected");
        // Usage was reconstructed, not reset: fill to the file cap counts the
        // pre-restart file.
        for _ in 0..MAX_FILES_PER_SESSION - 1 {
            store.stage("codex-keep", &png(1, 1, 40)).unwrap();
        }
        assert_eq!(store.stage("codex-keep", &png(1, 1, 40)), Err(StageError::Quota));
        let _ = std::fs::remove_dir_all(dir);
    }
}
