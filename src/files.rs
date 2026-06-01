// Filesystem endpoints — the $HOME-confined browse / editor / download block,
// a faithful port of the Go build's browse.go + files.go + editor.go. Every path
// goes through confine::resolve_under(home, …) so traversal can't escape $HOME.

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::confine::resolve_under;
use crate::engine::AppState;

const MAX_EDIT_BYTES: u64 = 5 << 20;

type R = Result<Response, (StatusCode, String)>;
fn e(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}

fn home(app: &AppState) -> PathBuf {
    app.inner.home.clone()
}

/// The share folder the Files view opens at by default: $CCWEB_SHARE_DIR or
/// ~/cc-share/, created on demand.
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

#[derive(Serialize)]
struct DirEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct FileEntry {
    name: String,
    path: String,
    size: i64,
    mtime: i64,
}

// ── GET /api/dirs ────────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    session: String,
}

pub async fn dirs(State(app): State<AppState>, Query(q): Query<PathQuery>) -> R {
    let home = home(&app);
    let dir = resolve_under(&home, &q.path).ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    let mut entries = read_dirs(&dir)?;
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(Json(json!({
        "path": dir.to_string_lossy(),
        "home": home.to_string_lossy(),
        "atHome": dir == home,
        "parent": dir.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "dirs": entries,
    }))
    .into_response())
}

fn read_dirs(dir: &Path) -> Result<Vec<DirEntry>, (StatusCode, String)> {
    let rd = std::fs::read_dir(dir).map_err(|err| e(StatusCode::BAD_REQUEST, err.to_string()))?;
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            out.push(DirEntry {
                name,
                path: ent.path().to_string_lossy().into_owned(),
            });
        }
    }
    Ok(out)
}

// ── GET /api/files ───────────────────────────────────────────────────────────
pub async fn files(State(app): State<AppState>, Query(q): Query<PathQuery>) -> R {
    let home = home(&app);
    let share = share_dir(&app);
    // Resolution order: ?path → ?session cwd → share dir.
    let mut q_path = q.path.trim().to_string();
    if q_path.is_empty() && !q.session.trim().is_empty() {
        let sess = app
            .get(q.session.trim())
            .ok_or_else(|| e(StatusCode::NOT_FOUND, "unknown session"))?;
        q_path = sess.live_cwd();
    }
    if q_path.is_empty() {
        q_path = share.to_string_lossy().into_owned();
    }
    let dir = resolve_under(&home, &q_path).ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;

    let rd = std::fs::read_dir(&dir).map_err(|err| e(StatusCode::BAD_REQUEST, err.to_string()))?;
    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut filev: Vec<FileEntry> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let full = ent.path();
        let Ok(meta) = ent.metadata() else { continue };
        if meta.is_dir() {
            dirs.push(DirEntry { name, path: full.to_string_lossy().into_owned() });
        } else if meta.is_file() {
            filev.push(FileEntry {
                name,
                path: full.to_string_lossy().into_owned(),
                size: meta.len() as i64,
                mtime: mtime_secs(&meta),
            });
        }
    }
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    filev.sort_by(|a, b| b.mtime.cmp(&a.mtime)); // newest first

    Ok(Json(json!({
        "path": dir.to_string_lossy(),
        "home": home.to_string_lossy(),
        "share": share.to_string_lossy(),
        "atHome": dir == home,
        "atShare": dir == share,
        "parent": dir.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "dirs": dirs,
        "files": filev,
    }))
    .into_response())
}

// ── GET /api/download ────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct DownloadQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    inline: String,
}

pub async fn download(
    State(app): State<AppState>,
    Query(q): Query<DownloadQuery>,
    headers: HeaderMap,
) -> R {
    let home = home(&app);
    let path = resolve_under(&home, &q.path).filter(|p| *p != home)
        .ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    let meta = std::fs::metadata(&path).map_err(|_| e(StatusCode::NOT_FOUND, "not found"))?;
    if !meta.is_file() {
        return Err(e(StatusCode::BAD_REQUEST, "not a regular file"));
    }
    let total = meta.len();
    let fname = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let disp = if q.inline == "1" { "inline" } else { "attachment" };
    let cd = format!("{disp}; filename*=UTF-8''{}", rfc5987(&fname));
    let ct = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    let open = |p: PathBuf| async move {
        tokio::fs::File::open(p).await.map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
    };

    // Honour a single byte-range so the PDF viewer (and any client) gets 206
    // partial content with Accept-Ranges — matching Go's http.ServeFile.
    if let Some((start, end)) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, total))
    {
        let len = end - start + 1;
        let mut file = open(path).await?;
        file.seek(SeekFrom::Start(start))
            .await
            .map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
        let body = Body::from_stream(ReaderStream::new(file.take(len)));
        return Ok(Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, ct)
            .header(header::CONTENT_DISPOSITION, cd)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{total}"))
            .header(header::CONTENT_LENGTH, len)
            .body(body)
            .unwrap());
    }

    let file = open(path).await?;
    let body = Body::from_stream(ReaderStream::new(file));
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ct)
        .header(header::CONTENT_DISPOSITION, cd)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total)
        .body(body)
        .unwrap())
}

/// Parse a single-range `Range: bytes=START-END` header (also `START-` and
/// `-SUFFIX`). Returns an inclusive (start, end) clamped to the file size, or
/// None if absent/unsatisfiable/multi-range (caller then serves the whole file).
fn parse_range(s: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = s.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multi-range not supported
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { total - 1 } else { b.parse::<u64>().ok()?.min(total - 1) };
        (start, end)
    };
    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

fn rfc5987(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        let unreserved = c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(c as char);
        } else {
            out.push('%');
            out.push(HEX[(c >> 4) as usize] as char);
            out.push(HEX[(c & 0xF) as usize] as char);
        }
    }
    out
}

// ── GET /api/file/read ───────────────────────────────────────────────────────
pub async fn file_read(State(app): State<AppState>, Query(q): Query<PathQuery>) -> R {
    let home = home(&app);
    let path = resolve_under(&home, &q.path).filter(|p| *p != home)
        .ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    let meta = std::fs::metadata(&path).map_err(|_| e(StatusCode::NOT_FOUND, "not found"))?;
    if !meta.is_file() {
        return Err(e(StatusCode::BAD_REQUEST, "not a regular file"));
    }
    if meta.len() > MAX_EDIT_BYTES {
        return Err(e(StatusCode::PAYLOAD_TOO_LARGE, "file too large to edit"));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    if !is_editable_text(&bytes) {
        return Ok((StatusCode::UNSUPPORTED_MEDIA_TYPE, Json(json!({ "editable": false }))).into_response());
    }
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Json(json!({
        "path": path.to_string_lossy(),
        "name": name,
        "content": String::from_utf8_lossy(&bytes),
        "size": meta.len() as i64,
        "mtime": mtime_secs(&meta),
    }))
    .into_response())
}

fn is_editable_text(b: &[u8]) -> bool {
    !b.contains(&0) && std::str::from_utf8(b).is_ok()
}

// ── POST /api/file/write ─────────────────────────────────────────────────────
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReq {
    path: String,
    content: String,
    #[serde(default)]
    base_mtime: i64,
}

pub async fn file_write(State(app): State<AppState>, Json(req): Json<WriteReq>) -> R {
    let home = home(&app);
    let path = resolve_under(&home, &req.path).filter(|p| *p != home)
        .ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    if req.content.len() as u64 > MAX_EDIT_BYTES {
        return Err(e(StatusCode::PAYLOAD_TOO_LARGE, "content too large"));
    }
    match std::fs::metadata(&path) {
        Ok(meta) => {
            if meta.is_dir() {
                return Err(e(StatusCode::BAD_REQUEST, "path is a directory"));
            }
            if req.base_mtime != 0 && mtime_secs(&meta) != req.base_mtime {
                return Err(e(StatusCode::CONFLICT, "file changed on disk"));
            }
        }
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            return Err(e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()));
        }
        Err(_) => {}
    }
    // Parent must already exist — don't conjure directory trees from a tap.
    let parent = path.parent().ok_or_else(|| e(StatusCode::BAD_REQUEST, "no parent"))?;
    if !parent.is_dir() {
        return Err(e(StatusCode::BAD_REQUEST, "parent folder does not exist"));
    }
    let tmp = path.with_extension("ccwtmp");
    tokio::fs::write(&tmp, req.content.as_bytes())
        .await
        .map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let meta = std::fs::metadata(&path).map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Json(json!({
        "path": path.to_string_lossy(),
        "name": name,
        "size": meta.len() as i64,
        "mtime": mtime_secs(&meta),
    }))
    .into_response())
}

// ── POST /api/file/delete ────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct PathReq {
    path: String,
}

pub async fn file_delete(State(app): State<AppState>, Json(req): Json<PathReq>) -> R {
    let home = home(&app);
    let path = resolve_under(&home, &req.path).filter(|p| *p != home)
        .ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    let meta = std::fs::metadata(&path).map_err(|_| e(StatusCode::NOT_FOUND, "not found"))?;
    if meta.is_dir() {
        return Err(e(StatusCode::BAD_REQUEST, "path is a directory"));
    }
    std::fs::remove_file(&path).map_err(|err| e(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── POST /api/mkdir ──────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct MkdirReq {
    dir: String,
    name: String,
}

pub async fn mkdir(State(app): State<AppState>, Json(req): Json<MkdirReq>) -> R {
    let home = home(&app);
    let dir = resolve_under(&home, &req.dir).ok_or_else(|| e(StatusCode::FORBIDDEN, "dir outside home"))?;
    let name = req.name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(e(StatusCode::BAD_REQUEST, "invalid folder name"));
    }
    let target = dir.join(name);
    match std::fs::create_dir(&target) {
        Ok(_) => Ok(Json(json!({ "name": name, "path": target.to_string_lossy() })).into_response()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(e(StatusCode::CONFLICT, "folder already exists"))
        }
        Err(err) => Err(e(StatusCode::BAD_REQUEST, err.to_string())),
    }
}

// ── POST /api/rmdir ──────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct RmdirReq {
    path: String,
    #[serde(default)]
    recursive: bool,
}

pub async fn rmdir(State(app): State<AppState>, Json(req): Json<RmdirReq>) -> R {
    let home = home(&app);
    let dir = resolve_under(&home, &req.path).ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    if dir == home {
        return Err(e(StatusCode::BAD_REQUEST, "refusing to delete home"));
    }
    if !dir.is_dir() {
        return Err(e(StatusCode::BAD_REQUEST, "not a directory"));
    }
    let res = if req.recursive {
        std::fs::remove_dir_all(&dir)
    } else {
        std::fs::remove_dir(&dir)
    };
    match res {
        Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(err) => {
            // ENOTEMPTY → 409 (non-recursive on a non-empty dir).
            if !req.recursive && err.raw_os_error() == Some(39) {
                Err(e(StatusCode::CONFLICT, "folder is not empty"))
            } else {
                Err(e(StatusCode::BAD_REQUEST, err.to_string()))
            }
        }
    }
}

// ── POST /api/rename ─────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct RenameReq {
    path: String,
    name: String,
}

pub async fn rename(State(app): State<AppState>, Json(req): Json<RenameReq>) -> R {
    let home = home(&app);
    let src = resolve_under(&home, &req.path).ok_or_else(|| e(StatusCode::FORBIDDEN, "path outside home"))?;
    if src == home {
        return Err(e(StatusCode::BAD_REQUEST, "refusing to rename home"));
    }
    let name = req.name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(e(StatusCode::BAD_REQUEST, "invalid name"));
    }
    if !src.exists() {
        return Err(e(StatusCode::NOT_FOUND, "not found"));
    }
    let parent = src.parent().ok_or_else(|| e(StatusCode::BAD_REQUEST, "no parent"))?;
    let dst = parent.join(name);
    // dst must stay under home and in the same parent (Join already cleaned it).
    if resolve_under(&home, &dst.to_string_lossy()).as_deref() != Some(dst.as_path()) {
        return Err(e(StatusCode::FORBIDDEN, "invalid destination"));
    }
    if dst == src {
        return Ok(Json(json!({ "name": name, "path": dst.to_string_lossy() })).into_response());
    }
    if dst.exists() {
        return Err(e(StatusCode::CONFLICT, "a file or folder with that name already exists"));
    }
    std::fs::rename(&src, &dst).map_err(|err| e(StatusCode::BAD_REQUEST, err.to_string()))?;
    Ok(Json(json!({ "name": name, "path": dst.to_string_lossy() })).into_response())
}
