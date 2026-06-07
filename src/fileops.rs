//! Agent-side dispatch for hub-routed small file-browser/editor ops. Mirrors the
//! REST handlers in `files.rs` (same JSON shapes the PWA expects) but returns a
//! [`CmdResult`] for the hub to relay. Confinement is the same authoritative
//! guard — every path goes through [`resolve_under`] so traversal can't escape
//! `$HOME`. Bulk transfers (download/upload/clipboard) are NOT here; they use the
//! dedicated bulk stream.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use cc_screen_protocol::hub::CmdResult;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::confine::{atomic_write, resolve_create_under, resolve_existing_under};
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
        "dirs_search" => dirs_search(app, args),
        "files" => files(app, args),
        "read" => read(app, args),
        "write" => write(app, args),
        "delete" => delete(app, args),
        "mkdir" => mkdir(app, args),
        "rmdir" => rmdir(app, args),
        "rename" => rename(app, args),
        "move" => move_path(app, args),
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
    let Some(dir) = resolve_existing_under(&home, &a.path) else {
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

#[derive(Deserialize, Default)]
struct DirSearchArgs {
    #[serde(default)]
    q: String,
    #[serde(default)]
    root: String,
}

/// Recursive fuzzy dir search (proposal 0016), hub-relayed mirror of
/// files.rs::dirs_search. Same confinement + ranking via the shared core.
fn dirs_search(app: &AppState, args: Value) -> CmdResult {
    let a: DirSearchArgs = serde_json::from_value(args).unwrap_or_default();
    let home = home(app);
    let Some(root) = resolve_existing_under(&home, &a.root) else {
        return err(403, "path outside home");
    };
    let recent: HashSet<PathBuf> = app.list().iter().map(|s| PathBuf::from(s.live_cwd())).collect();
    let hits = crate::dirsearch::search(&home, &root, &a.q, &recent);
    CmdResult::Json(crate::dirsearch::results_json(&home, &root, &hits))
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
    let Some(dir) = resolve_existing_under(&home, &q_path) else {
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
    resolve_existing_under(&home, raw)
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
    // A write may create a new leaf, so confine by the (canonical) parent — but
    // still forbid targeting $HOME itself.
    let home = home(app);
    let path = match resolve_create_under(&home, &a.path).filter(|p| *p != home) {
        Some(p) => p,
        None => return err(403, "path outside home"),
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
    if let Err(e) = atomic_write(&path, a.content.as_bytes()) {
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
    let Some(dir) = resolve_existing_under(&home, &a.dir) else {
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
    let Some(dir) = resolve_existing_under(&home, &a.path) else {
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
    let Some(src) = resolve_existing_under(&home, &a.path) else {
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
    if resolve_create_under(&home, &dst.to_string_lossy()).as_deref() != Some(dst.as_path()) {
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

#[derive(Deserialize)]
struct MoveArgs {
    path: String,
    dest: String,
}

// Mirror of files.rs::move_path for the hub relay (proposal 0012). Same
// confinement + descendant guard so the `?machine=` path matches the direct one.
fn move_path(app: &AppState, args: Value) -> CmdResult {
    let a: MoveArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return err(400, e.to_string()),
    };
    let home = home(app);
    let Some(src) = resolve_existing_under(&home, &a.path) else {
        return err(403, "path outside home");
    };
    let Some(dst_dir) = resolve_existing_under(&home, &a.dest) else {
        return err(403, "dest outside home");
    };
    if src == home {
        return err(400, "refusing to move home");
    }
    if !dst_dir.is_dir() {
        return err(400, "destination is not a directory");
    }
    let Some(name) = src.file_name() else { return err(400, "no name") };
    let dst = dst_dir.join(name);
    if resolve_create_under(&home, &dst.to_string_lossy()).as_deref() != Some(dst.as_path()) {
        return err(403, "invalid destination");
    }
    let name = name.to_string_lossy();
    if dst == src {
        return CmdResult::Json(json!({ "name": name, "path": dst.to_string_lossy() }));
    }
    if dst.starts_with(&src) {
        return err(400, "cannot move a folder into itself");
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
    fn move_path_cases() {
        // happy-path file move, happy-path dir move, collision (409),
        // move-into-descendant (400), outside-$HOME (403). (proposal 0012)
        let tmp = std::env::temp_dir().join(format!("ccr-fileops-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let a = tmp.join("a");
        let b = tmp.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let app = app(&tmp);

        // happy path: move a file from a/ into b/.
        std::fs::write(a.join("f.txt"), b"hi").unwrap();
        let r = run(&app, "move", json!({ "path": a.join("f.txt").to_string_lossy(), "dest": b.to_string_lossy() }));
        assert!(matches!(r, CmdResult::Json(_)), "file move ok: {r:?}");
        assert!(b.join("f.txt").exists() && !a.join("f.txt").exists());

        // happy path: move a whole directory (with contents) into b/.
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::write(a.join("sub/inner.txt"), b"x").unwrap();
        let r = run(&app, "move", json!({ "path": a.join("sub").to_string_lossy(), "dest": b.to_string_lossy() }));
        assert!(matches!(r, CmdResult::Json(_)), "dir move ok: {r:?}");
        assert!(b.join("sub/inner.txt").exists() && !a.join("sub").exists());

        // collision: a file named f.txt already exists in b/ → 409.
        std::fs::write(a.join("f.txt"), b"again").unwrap();
        let r = run(&app, "move", json!({ "path": a.join("f.txt").to_string_lossy(), "dest": b.to_string_lossy() }));
        assert!(matches!(r, CmdResult::Error { code: 409, .. }), "collision → 409: {r:?}");
        assert!(a.join("f.txt").exists(), "source untouched on collision");

        // move a folder into its own descendant → 400.
        std::fs::create_dir_all(b.join("sub/deep")).unwrap();
        let r = run(&app, "move", json!({ "path": b.join("sub").to_string_lossy(), "dest": b.join("sub/deep").to_string_lossy() }));
        assert!(matches!(r, CmdResult::Error { code: 400, .. }), "into descendant → 400: {r:?}");

        // destination outside $HOME → 403.
        let r = run(&app, "move", json!({ "path": a.join("f.txt").to_string_lossy(), "dest": "/tmp" }));
        assert!(matches!(r, CmdResult::Error { code: 403, .. }), "dest outside home → 403: {r:?}");

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

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_home_is_rejected() {
        // A symlink under home pointing OUTSIDE must not be a read/write/list/delete
        // bypass, while one pointing back inside still works.
        let base = std::env::temp_dir().join(format!("ccr-fileops-escape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let outside = base.join("outside");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"top secret").unwrap();
        std::os::unix::fs::symlink(&outside, home.join("escape")).unwrap();
        let app = app(&home);

        let escaped = home.join("escape/secret.txt").to_string_lossy().into_owned();
        // read / delete through the outward symlink → 403.
        assert!(matches!(run(&app, "read", json!({ "path": escaped })), CmdResult::Error { code: 403, .. }));
        assert!(matches!(run(&app, "delete", json!({ "path": escaped })), CmdResult::Error { code: 403, .. }));
        // listing the outward-symlinked dir → 403.
        assert!(matches!(
            run(&app, "dirs", json!({ "path": home.join("escape").to_string_lossy() })),
            CmdResult::Error { code: 403, .. }
        ));
        // writing a NEW file through the outward symlink → 403; the outside dir is untouched.
        let new_escaped = home.join("escape/planted.txt").to_string_lossy().into_owned();
        assert!(matches!(
            run(&app, "write", json!({ "path": new_escaped, "content": "x" })),
            CmdResult::Error { code: 403, .. }
        ));
        assert!(!outside.join("planted.txt").exists(), "nothing written outside home");

        let _ = std::fs::remove_dir_all(&base);
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
