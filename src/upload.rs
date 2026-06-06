// Drag-and-drop upload — port of the Go upload.go. Destination root is the
// session's cwd when a session is named (terminal-pane drop), else $HOME (the
// editor file-tree drop); resolve_under re-checks the concrete dir either way.
// Folder structure is preserved because the client sends each part's filename as
// a relpath ("src/icons/foo.svg"); axum/multer keep it verbatim (unlike Go,
// which we had to work around). Per-file outcomes go in the JSON response so a
// single bad part doesn't sink the whole upload.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::{
    extract::{Multipart, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;

use crate::confine::{resolve_create_under, resolve_existing_under, safe_rel};
use crate::engine::AppState;

type R = Result<Response, (StatusCode, String)>;
fn e(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}

fn upload_root(app: &AppState, session: &str) -> Result<PathBuf, (StatusCode, String)> {
    let session = session.trim();
    if session.is_empty() {
        return Ok(app.inner.home.clone());
    }
    let sess = app.get(session).ok_or_else(|| e(StatusCode::NOT_FOUND, "unknown session"))?;
    resolve_existing_under(&app.inner.home, &sess.live_cwd())
        .ok_or_else(|| e(StatusCode::FORBIDDEN, "session cwd outside home"))
}

// ── POST /api/upload/check ───────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct CheckReq {
    #[serde(default)]
    session: String,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    names: Vec<String>,
}

pub async fn upload_check(State(app): State<AppState>, Json(req): Json<CheckReq>) -> R {
    let home = app.inner.home.clone();
    let root = upload_root(&app, &req.session)?;
    let dir = resolve_existing_under(&root, &req.dir).ok_or_else(|| e(StatusCode::FORBIDDEN, "dir outside allowed root"))?;
    let mut exists: Vec<String> = Vec::new();
    for n in &req.names {
        let Some(rel) = safe_rel(n) else { continue };
        let target = dir.join(&rel);
        // Symlink-safe: the target's real parent must stay under $HOME.
        if resolve_create_under(&home, &target.to_string_lossy()).as_deref() != Some(target.as_path()) {
            continue;
        }
        if target.exists() {
            exists.push(rel.to_string_lossy().into_owned());
        }
    }
    Ok(Json(json!({ "exists": exists })).into_response())
}

// ── POST /api/upload ─────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    session: String,
    #[serde(default)]
    dir: String,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    items: Vec<MItem>,
}
#[derive(Deserialize)]
struct MItem {
    name: String,
    mode: String,
}

#[derive(Serialize, Default)]
struct UploadResp {
    written: Vec<String>,
    renamed: HashMap<String, String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    errors: HashMap<String, String>,
}

pub async fn upload(
    State(app): State<AppState>,
    Query(q): Query<UploadQuery>,
    mut mp: Multipart,
) -> R {
    let home = app.inner.home.clone();
    let root = upload_root(&app, &q.session)?;
    let dir = resolve_existing_under(&root, &q.dir).ok_or_else(|| e(StatusCode::FORBIDDEN, "dir outside allowed root"))?;
    if !dir.is_dir() {
        return Err(e(StatusCode::BAD_REQUEST, "dir does not exist"));
    }

    let mut modes: HashMap<String, String> = HashMap::new();
    let mut resp = UploadResp::default();

    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(err) => {
                resp.errors.insert("_stream".into(), err.to_string());
                break;
            }
        };
        let mut field = field;
        let fieldname = field.name().unwrap_or("").to_string();

        if fieldname == "manifest" {
            if let Ok(data) = field.bytes().await {
                if let Ok(m) = serde_json::from_slice::<Manifest>(&data) {
                    for it in m.items {
                        if it.mode == "overwrite" || it.mode == "rename" {
                            if let Some(rel) = safe_rel(&it.name) {
                                modes.insert(rel.to_string_lossy().into_owned(), it.mode);
                            }
                        }
                    }
                }
            }
            continue;
        }
        if fieldname != "file" {
            continue;
        }

        let raw = field.file_name().map(|s| s.to_string()).unwrap_or_default();
        let Some(rel) = safe_rel(&raw) else {
            resp.errors.insert(raw, "invalid path".into());
            continue;
        };
        let rel_str = rel.to_string_lossy().into_owned();
        let target = dir.join(&rel);
        // Symlink-safe: the target's nearest existing ancestor must stay under
        // $HOME — rejecting an upload routed through a symlinked dir pointing out,
        // BEFORE we create any intermediate directories.
        if resolve_create_under(&home, &target.to_string_lossy()).as_deref() != Some(target.as_path()) {
            resp.errors.insert(rel_str, "path escapes home".into());
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                resp.errors.insert(rel_str, format!("mkdir: {err}"));
                continue;
            }
        }

        let mode = modes.get(&rel_str).map(String::as_str).unwrap_or("rename");
        let mut final_abs = target.clone();
        let mut final_rel = rel_str.clone();
        if target.exists() {
            match mode {
                "overwrite" => {}
                "rename" => {
                    final_abs = next_available(&target);
                    final_rel = final_abs
                        .strip_prefix(&dir)
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| rel_str.clone());
                    resp.renamed.insert(rel_str.clone(), final_rel.clone());
                }
                _ => {
                    resp.errors.insert(rel_str, "unknown mode".into());
                    continue;
                }
            }
        }

        match write_field(&mut field, &final_abs).await {
            Ok(_) => resp.written.push(final_rel),
            Err(err) => {
                resp.errors.insert(rel_str, err);
            }
        }
    }

    Ok(Json(resp).into_response())
}

async fn write_field(field: &mut axum::extract::multipart::Field<'_>, path: &Path) -> Result<(), String> {
    let mut f = tokio::fs::File::create(path).await.map_err(|e| format!("create: {e}"))?;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => f.write_all(&chunk).await.map_err(|e| format!("write: {e}"))?,
            Ok(None) => break,
            Err(err) => {
                let _ = tokio::fs::remove_file(path).await;
                return Err(format!("read: {err}"));
            }
        }
    }
    f.flush().await.map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Pick an unused "foo-N.ext" in the same dir (suffix before the final ext).
fn next_available(p: &Path) -> PathBuf {
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    for i in 1..=10000 {
        let name = match &ext {
            Some(ext) => format!("{stem}-{i}.{ext}"),
            None => format!("{stem}-{i}"),
        };
        let cand = parent.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    p.to_path_buf()
}
