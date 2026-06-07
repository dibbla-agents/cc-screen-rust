// Session persistence manifest — the resume-after-restart store. Far simpler
// than the Go build's, because the engine spawns every session itself and so
// observes each child's exit status directly (no on-disk exit markers, no
// seenLive set, no reconcile of shell-started sessions):
//
//   - create  → record an entry
//   - clean exit (the user typed /exit; child exited 0) → forget it (the reaper)
//   - web delete (exit or kill) → forget it (the handler)
//   - crash / signal / backend redeploy → entry LEFT in place → restorable
//
// On a backend restart the process dies before any reaper runs, so entries
// persist untouched and auto-restore-on-startup brings them back (resuming each
// CLI's conversation). Single file, mutex-serialised, atomic temp+rename.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

static LOCK: Mutex<()> = Mutex::new(());
const MAX: usize = 100;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub session: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cmd: String,
    pub prefix: String,
    pub short: String,
    pub dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_dirs: Vec<String>,
    pub created_at: i64,
    /// Per-session launch policy (proposal 0005), persisted so a redeploy
    /// relaunches each session under the same policy. Old `sessions.json` files
    /// lacking it default to skip-on — today's behavior. (The `remoteControl`
    /// field once stored here is retired by 0014: a stray one in an old file is
    /// ignored, and the session restores editable.)
    #[serde(default = "default_true")]
    pub skip_permissions: bool,
}

/// Default for `Entry::skip_permissions` — a pre-0005 manifest entry was always
/// launched YOLO, so a missing field restores as YOLO.
pub fn default_true() -> bool {
    true
}

fn file(config_dir: &Path) -> PathBuf {
    config_dir.join("sessions.json")
}

fn load_locked(config_dir: &Path) -> Vec<Entry> {
    std::fs::read_to_string(file(config_dir))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Entry>>(&s).ok())
        .unwrap_or_default()
}

fn save_locked(config_dir: &Path, mut entries: Vec<Entry>) {
    if entries.len() > MAX {
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        entries.truncate(MAX);
    }
    let path = file(config_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(b) = serde_json::to_vec_pretty(&entries) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &b).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Upsert an entry by session name. Best-effort; never blocks session creation.
pub fn record(config_dir: &Path, e: Entry) {
    let _g = LOCK.lock().unwrap();
    let mut entries = load_locked(config_dir);
    if let Some(slot) = entries.iter_mut().find(|x| x.session == e.session) {
        *slot = e;
    } else {
        entries.push(e);
    }
    save_locked(config_dir, entries);
}

/// Drop an entry — a deliberate end (clean /exit or a web delete) that must not
/// come back on the next restore.
pub fn forget(config_dir: &Path, session: &str) {
    let _g = LOCK.lock().unwrap();
    let mut entries = load_locked(config_dir);
    let before = entries.len();
    entries.retain(|x| x.session != session);
    if entries.len() != before {
        save_locked(config_dir, entries);
    }
}

pub fn entries(config_dir: &Path) -> Vec<Entry> {
    let _g = LOCK.lock().unwrap();
    load_locked(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(session: &str) -> Entry {
        Entry {
            session: session.into(),
            cmd: "cc".into(),
            prefix: "claude".into(),
            short: session.trim_start_matches("claude-").into(),
            dir: "/tmp".into(),
            extra_dirs: vec![],
            created_at: 1,
            skip_permissions: true,
        }
    }

    #[test]
    fn record_upsert_forget() {
        let dir = std::env::temp_dir().join(format!("ccr-mtest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        record(&dir, entry("claude-a"));
        record(&dir, entry("claude-a")); // upsert, not duplicate
        record(&dir, entry("claude-b"));
        assert_eq!(entries(&dir).len(), 2);
        forget(&dir, "claude-a");
        let left = entries(&dir);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].session, "claude-b");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_entry_defaults_skip_on_and_ignores_remote_control() {
        // A pre-0005 sessions.json omits skipPermissions → defaults to skip on.
        let json = r#"[{"session":"claude-a","cmd":"cc","prefix":"claude","short":"a","dir":"/tmp","createdAt":1}]"#;
        let e: Vec<Entry> = serde_json::from_str(json).unwrap();
        assert!(e[0].skip_permissions);
        // A 0005-era entry persisted as view-only (`remoteControl: false`) still
        // deserializes — the retired field is ignored (0014) — and there is no
        // longer any policy to restore it under, so the session comes back editable.
        let legacy = r#"[{"session":"claude-b","prefix":"claude","short":"b","dir":"/tmp","createdAt":2,"skipPermissions":true,"remoteControl":false}]"#;
        let e2: Vec<Entry> = serde_json::from_str(legacy).unwrap();
        assert_eq!(e2[0].session, "claude-b");
        assert!(e2[0].skip_permissions);
        // Round-trips as camelCase, with no remoteControl field written back.
        let v = serde_json::to_string(&e[0]).unwrap();
        assert!(v.contains(r#""skipPermissions":true"#), "{v}");
        assert!(!v.contains("remoteControl"), "retired field must not serialize: {v}");
    }
}
