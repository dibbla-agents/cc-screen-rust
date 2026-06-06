//! Agent-side dispatch for hub-routed small file-browser/editor ops. Mirrors the
//! REST handlers in `files.rs` (same JSON shapes the PWA expects) but returns a
//! [`CmdResult`] for the hub to relay. Confinement is the same authoritative
//! guard — every path goes through [`resolve_under`] so traversal can't escape
//! `$HOME`. Bulk transfers (download/upload/clipboard) are NOT here; they use the
//! dedicated bulk stream.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use cc_screen_protocol::hub::CmdResult;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::confine::resolve_under;
use crate::engine::AppState;

const MAX_EDIT_BYTES: u64 = 5 << 20;

fn err(code: u16, msg: impl Into<String>) -> CmdResult {
    CmdResult::Error { code, msg: msg.into() }
}

fn home(app: &AppState) -> PathBuf {
    app.inner.home.clone()
}

fn share_dir(app: &AppState) -> PathBuf {
    if let Ok(d) = std::env::var("CCWEB_SHARE_DIR") {
        let d = d.trim();
        if !d.is_empty() {
            let p = PathBuf::from(d);
            let _ = std::fs::create_dir_all(&p);
            return p;
        }
    }
    let p = home(app).join("cc-share");
    let _ = std::fs::create_dir_all(&p);
    p
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn dir_entry(name: String, path: &Path) -> Value {
    json!({ "name": name, "path": path.to_string_lossy() })
}

/// Run a file op by name. Unknown ops / bad args produce a 400.
pub fn run(app: &AppState, op: &str, args: Value) -> CmdResult {
    match op {
        "dirs" => dirs(app, args),
        "files" => files(app, args),
        "read" => read(app, args),
        "write" => write(app, args),
        "delete" => delete(app, args),
        "mkdir" => mkdir(app, args),
        "rmdir" => rmdir(app, args),
        "rename" => rename(app, args),
        _ => err(400, format!("unknown file op: {op}")),
    }
}

#[derive(Deserialize, Default)]
struct PathArgs {
    #[serde(default)]
    path: String,
    #[serde(default)]
    session: String,
}

fn dirs(app: &AppState, args: Value) -> CmdResult {
    let a: PathArgs = serde_json::from_value(args).unwrap_or_default();
    let home = home(app);
    let Some(dir) = resolve_under(&home, &a.path) else {
        return err(403, "path outside home");
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => return err(400, e.to_string()),
    };
    // follow symlinks via fs::metadata so a symlinked dir lists as a folder
    // (broken links fail metadata() and are skipped).
    let mut entries: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let dirs: Vec<Value> = entries.iter().map(|(n, p)| dir_entry(n.clone(), p)).collect();
    CmdResult::Json(json!({
        "path": dir.to_string_lossy(),
        "home": home.to_string_lossy(),
        "atHome": dir == home,
        "parent": dir.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "dirs": dirs,
    }))
}

fn files(app: &AppState, args: Value) -> CmdResult {
    let a: PathArgs = serde_json::from_value(args).unwrap_or_default();
    let home = home(app);
    let share = share_dir(app);
    // Resolution order: path → session cwd → share dir.
    let mut q_path = a.path.trim().to_string();
    if q_path.is_empty() && !a.session.trim().is_empty() {
        match app.get(a.session.trim()) {
            Some(sess) => q_path = sess.live_cwd(),
            None => return err(404, "unknown session"),
        }
    }
    if q_path.is_empty() {
        q_path = share.to_string_lossy().into_owned();
    }
    let Some(dir) = resolve_under(&home, &q_path) else {
        return err(403, "path outside home");
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => return err(400, e.to_string()),
    };
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut filev: Vec<(String, PathBuf, i64, i64)> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let full = ent.path();
        // follow symlinks so symlinked dirs/files resolve to their target type
        // (broken links fail metadata() and are skipped).
        let Ok(meta) = std::fs::metadata(&full) else { continue };
        if meta.is_dir() {
            dirs.push((name, full));
        } else if meta.is_file() {
            filev.push((name, full, meta.len() as i64, mtime_secs(&meta)));
        }
    }
    dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    filev.sort_by(|a, b| b.3.cmp(&a.3)); // newest first
    let dirs_json: Vec<Value> = dirs.iter().map(|(n, p)| dir_entry(n.clone(), p)).collect();
    let files_json: Vec<Value> = filev
        .iter()
        .map(|(n, p, sz, mt)| json!({ "name": n, "path": p.to_string_lossy(), "size": sz, "mtime": mt }))
        .collect();
    CmdResult::Json(json!({
        "path": dir.to_string_lossy(),
        "home": home.to_string_lossy(),
        "share": share.to_string_lossy(),
        "atHome": dir == home,
        "atShare": dir == share,
        "parent": dir.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "dirs": dirs_json,
        "files": files_json,
    }))
}

/// A path arg that must resolve under home AND not be home itself (read/write/
/// delete target a file, never the home dir).
fn resolve_file(app: &AppState, raw: &str) -> Result<PathBuf, CmdResult> {
    let home = home(app);
    resolve_under(&home, raw)
        .filter(|p| *p != home)
        .ok_or_else(|| err(403, "path outside home"))
}

fn read(app: &AppState, args: Value) -> CmdResult {
    let a: PathArgs = serde_json::from_value(args).unwrap_or_default();
    let path = match resolve_file(app, &a.path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return err(404, "not found"),
    };
    if !meta.is_file() {
        return err(400, "not a regular file");
    }
    if meta.len() > MAX_EDIT_BYTES {
        return err(413, "file too large to edit");
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return err(500, e.to_string()),
    };
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return CmdResult::Json(json!({ "editable": false }));
    }
    CmdResult::Json(json!({
        "path": path.to_string_lossy(),
        "name": path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        "content": String::from_utf8_lossy(&bytes),
        "size": meta.len() as i64,
        "mtime": mtime_secs(&meta),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    base_mtime: i64,
}

fn write(app: &AppState, args: Value) -> CmdResult {
    let a: WriteArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return err(400, e.to_string()),
    };
    let path = match resolve_file(app, &a.path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    if a.content.len() as u64 > MAX_EDIT_BYTES {
        return err(413, "content too large");
    }
    match std::fs::metadata(&path) {
        Ok(meta) => {
            if meta.is_dir() {
                return err(400, "path is a directory");
            }
            if a.base_mtime != 0 && mtime_secs(&meta) != a.base_mtime {
                return err(409, "file changed on disk");
            }
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return err(500, e.to_string()),
        Err(_) => {}
    }
    let Some(parent) = path.parent() else { return err(400, "no parent") };
    if !parent.is_dir() {
        return err(400, "parent folder does not exist");
    }
    let tmp = path.with_extension("ccwtmp");
    if let Err(e) = std::fs::write(&tmp, a.content.as_bytes()) {
        return err(500, e.to_string());
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        return err(500, e.to_string());
    }
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => return err(500, e.to_string()),
    };
    CmdResult::Json(json!({
        "path": path.to_string_lossy(),
        "name": path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        "size": meta.len() as i64,
        "mtime": mtime_secs(&meta),
    }))
}

fn delete(app: &AppState, args: Value) -> CmdResult {
    let a: PathArgs = serde_json::from_value(args).unwrap_or_default();
    let path = match resolve_file(app, &a.path) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return err(404, "not found"),
    };
    if meta.is_dir() {
        return err(400, "path is a directory");
    }
    match std::fs::remove_file(&path) {
        Ok(()) => CmdResult::Ok,
        Err(e) => err(500, e.to_string()),
    }
}

#[derive(Deserialize)]
struct MkdirArgs {
    dir: String,
    name: String,
}

fn mkdir(app: &AppState, args: Value) -> CmdResult {
    let a: MkdirArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return err(400, e.to_string()),
    };
    let home = home(app);
    let Some(dir) = resolve_under(&home, &a.dir) else {
        return err(403, "dir outside home");
    };
    let name = a.name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return err(400, "invalid folder name");
    }
    let target = dir.join(name);
    match std::fs::create_dir(&target) {
        Ok(()) => CmdResult::Json(json!({ "name": name, "path": target.to_string_lossy() })),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => err(409, "folder already exists"),
        Err(e) => err(400, e.to_string()),
    }
}

#[derive(Deserialize)]
struct RmdirArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

fn rmdir(app: &AppState, args: Value) -> CmdResult {
    let a: RmdirArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return err(400, e.to_string()),
    };
    let home = home(app);
    let Some(dir) = resolve_under(&home, &a.path) else {
        return err(403, "path outside home");
    };
    if dir == home {
        return err(400, "refusing to delete home");
    }
    if !dir.is_dir() {
        return err(400, "not a directory");
    }
    let res = if a.recursive { std::fs::remove_dir_all(&dir) } else { std::fs::remove_dir(&dir) };
    match res {
        Ok(()) => CmdResult::Ok,
        Err(e) if !a.recursive && e.raw_os_error() == Some(39) => err(409, "folder is not empty"),
        Err(e) => err(400, e.to_string()),
    }
}

#[derive(Deserialize)]
struct RenameArgs {
    path: String,
    name: String,
}

fn rename(app: &AppState, args: Value) -> CmdResult {
    let a: RenameArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return err(400, e.to_string()),
    };
    let home = home(app);
    let Some(src) = resolve_under(&home, &a.path) else {
        return err(403, "path outside home");
    };
    if src == home {
        return err(400, "refusing to rename home");
    }
    let name = a.name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return err(400, "invalid name");
    }
    if !src.exists() {
        return err(404, "not found");
    }
    let Some(parent) = src.parent() else { return err(400, "no parent") };
    let dst = parent.join(name);
    if resolve_under(&home, &dst.to_string_lossy()).as_deref() != Some(dst.as_path()) {
        return err(403, "invalid destination");
    }
    if dst == src {
        return CmdResult::Json(json!({ "name": name, "path": dst.to_string_lossy() }));
    }
    if dst.exists() {
        return err(409, "a file or folder with that name already exists");
    }
    match std::fs::rename(&src, &dst) {
        Ok(()) => CmdResult::Json(json!({ "name": name, "path": dst.to_string_lossy() })),
        Err(e) => err(400, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(tmp: &Path) -> AppState {
        AppState::new(
            vec![],
            String::new(),
            tmp.to_path_buf(),
            tmp.to_path_buf(),
            "test-agent".into(),
            crate::auth::Auth::load(tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        )
    }

    #[test]
    fn mkdir_list_write_read_rename_delete_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ccr-fileops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let app = app(&tmp);

        // mkdir under home
        let r = run(&app, "mkdir", json!({ "dir": tmp.to_string_lossy(), "name": "proj" }));
        assert!(matches!(r, CmdResult::Json(_)), "mkdir ok: {r:?}");

        // write a file in it
        let fpath = tmp.join("proj").join("a.txt");
        let r = run(&app, "write", json!({ "path": fpath.to_string_lossy(), "content": "hello" }));
        assert!(matches!(r, CmdResult::Json(_)), "write ok: {r:?}");

        // read it back
        let r = run(&app, "read", json!({ "path": fpath.to_string_lossy() }));
        match r {
            CmdResult::Json(v) => assert_eq!(v["content"], "hello"),
            other => panic!("read: {other:?}"),
        }

        // dirs lists "proj"
        let r = run(&app, "dirs", json!({ "path": tmp.to_string_lossy() }));
        match r {
            CmdResult::Json(v) => {
                let names: Vec<String> =
                    v["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
                assert!(names.contains(&"proj".to_string()));
            }
            other => panic!("dirs: {other:?}"),
        }

        // rename the file
        let r = run(&app, "rename", json!({ "path": fpath.to_string_lossy(), "name": "b.txt" }));
        assert!(matches!(r, CmdResult::Json(_)), "rename ok: {r:?}");

        // delete it
        let bpath = tmp.join("proj").join("b.txt");
        let r = run(&app, "delete", json!({ "path": bpath.to_string_lossy() }));
        assert!(matches!(r, CmdResult::Ok), "delete → 204: {r:?}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dirs_follows_symlinked_directories() {
        // An isolated-$HOME agent fills its home with symlinks back to the real
        // home; a symlinked dir must still list as a folder (the file_type()
        // path used to drop them because it never traverses the link).
        let tmp = std::env::temp_dir().join(format!("ccr-fileops-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let real = tmp.join("real_target");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        let app = app(&home);

        std::os::unix::fs::symlink(&real, home.join("linked")).unwrap();
        // a dangling link must be skipped, not error the listing
        std::os::unix::fs::symlink(tmp.join("missing"), home.join("dangling")).unwrap();

        for op in ["dirs", "files"] {
            let r = run(&app, op, json!({ "path": home.to_string_lossy() }));
            match r {
                CmdResult::Json(v) => {
                    let names: Vec<String> = v["dirs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|d| d["name"].as_str().unwrap().to_string())
                        .collect();
                    assert!(names.contains(&"linked".to_string()), "{op}: symlinked dir missing: {names:?}");
                    assert!(!names.contains(&"dangling".to_string()), "{op}: dangling link should be skipped");
                }
                other => panic!("{op}: {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn confinement_rejects_paths_outside_home() {
        let tmp = std::env::temp_dir().join(format!("ccr-fileops-confine-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let app = app(&tmp);
        // Absolute escape and a traversal both rejected (403).
        for bad in ["/etc", "../../etc"] {
            let r = run(&app, "dirs", json!({ "path": bad }));
            assert!(matches!(r, CmdResult::Error { code: 403, .. }), "{bad} should be 403: {r:?}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unknown_op_is_400() {
        let tmp = std::env::temp_dir().join(format!("ccr-fileops-unknown-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let app = app(&tmp);
        assert!(matches!(run(&app, "frobnicate", json!({})), CmdResult::Error { code: 400, .. }));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
