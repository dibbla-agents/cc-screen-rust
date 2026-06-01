// HTTP + WebSocket handlers. These implement the exact wire contract the
// existing React frontend already speaks (see web/frontend/src/api.ts in the Go
// repo), so the UI runs against this backend nearly unchanged. Render (binary
// PTY bytes over the WS) and input (key/paste, separate POSTs) are independent
// channels, exactly as before.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::confine::resolve_under;
use crate::engine::{key_bytes, AppState, Session};
use crate::tools::Tool;

#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

type ApiResult = Result<Response, (StatusCode, String)>;

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}

// ── GET /api/sessions ────────────────────────────────────────────────────────
#[derive(Serialize)]
pub struct SessionDto {
    name: String,
    tool: String,
    short: String,
    attached: bool,
    activity: i64,
    preview: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    cwd: String,
}

pub async fn sessions(State(app): State<AppState>) -> Json<Vec<SessionDto>> {
    let dtos = app
        .list()
        .into_iter()
        .map(|s| SessionDto {
            name: s.name.clone(),
            tool: s.tool.clone(),
            short: s.short.clone(),
            attached: s.attached(),
            activity: s.last_activity() as i64,
            preview: s.preview(),
            cwd: s.live_cwd(),
        })
        .collect();
    Json(dtos)
}

// ── GET /api/tools ───────────────────────────────────────────────────────────
pub async fn tools(State(app): State<AppState>) -> Json<Value> {
    let list: Vec<Value> = app
        .inner
        .tools
        .iter()
        .map(|t| {
            let mut o = json!({ "cmd": t.cmd, "prefix": t.prefix });
            if t.extra_flag.is_some() {
                let mut ed = json!({});
                if t.extra_max > 0 {
                    ed["max"] = json!(t.extra_max);
                }
                o["extraDirs"] = ed;
            }
            o
        })
        .collect();
    Json(json!(list))
}

// ── POST /api/session ────────────────────────────────────────────────────────
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReq {
    tool: String,
    name: String,
    dir: String,
    #[serde(default)]
    extra_dirs: Vec<String>,
}

pub async fn create_session(State(app): State<AppState>, Json(req): Json<CreateReq>) -> ApiResult {
    let tool = app
        .find_tool(&req.tool)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown tool: {}", req.tool)))?;
    // Confine the working dir to $HOME (the agents run YOLO — never let a client
    // anchor a session at /etc), and confirm it's a real directory.
    let home = app.inner.home.clone();
    let dir = resolve_under(&home, &req.dir).ok_or_else(|| err(StatusCode::FORBIDDEN, "dir outside home"))?;
    if !dir.is_dir() {
        return Err(err(StatusCode::BAD_REQUEST, "not a directory"));
    }
    let extra = validate_extra_dirs(&app, &tool, &dir, &req.extra_dirs)?;
    // Empty name defaults to the dir's basename (mirrors the Go build).
    let mut name = req.name.trim().to_string();
    if name.is_empty() {
        name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    match app.create(&tool, &name, &dir.to_string_lossy(), extra, false) {
        Ok(full) => Ok(Json(json!({ "name": full })).into_response()),
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            Err(err(code, msg))
        }
    }
}

// Confine + dedupe extra workspace dirs and enforce the tool's max (mirrors the
// Go validateExtraDirs). The primary dir and duplicates are dropped.
fn validate_extra_dirs(
    app: &AppState,
    tool: &Tool,
    primary: &Path,
    requested: &[String],
) -> Result<Vec<String>, (StatusCode, String)> {
    if requested.is_empty() {
        return Ok(Vec::new());
    }
    if tool.extra_flag.is_none() {
        return Err(err(StatusCode::BAD_REQUEST, "tool does not support extra folders"));
    }
    let home = &app.inner.home;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for raw in requested {
        let d = resolve_under(home, raw).ok_or_else(|| err(StatusCode::FORBIDDEN, "extra folder outside home"))?;
        if d == primary || !seen.insert(d.clone()) {
            continue;
        }
        if !d.is_dir() {
            return Err(err(StatusCode::BAD_REQUEST, "extra folder is not a directory"));
        }
        out.push(d.to_string_lossy().into_owned());
    }
    if tool.extra_max > 0 && out.len() > tool.extra_max as usize {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("{} supports at most {} extra folders", tool.prefix, tool.extra_max),
        ));
    }
    Ok(out)
}

// ── POST /api/session/delete ─────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct DeleteReq {
    session: String,
    #[serde(default)]
    mode: String, // "exit" (graceful) | "kill" (hard)
}

pub async fn delete_session(State(app): State<AppState>, Json(req): Json<DeleteReq>) -> ApiResult {
    let sess = app
        .get(&req.session)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown session"))?;
    // The user is ending it on purpose — drop it from the restore manifest so a
    // later restore doesn't resurrect it.
    crate::manifest::forget(&app.inner.config_dir, &req.session);
    match req.mode.as_str() {
        "exit" | "soft" => {
            // Inject /exit; the child exits asynchronously, so the client polls
            // /api/sessions until it's gone. 202 Accepted.
            sess.graceful_exit();
            Ok(StatusCode::ACCEPTED.into_response())
        }
        "kill" | "hard" | "" => {
            sess.kill();
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        _ => Err(err(StatusCode::BAD_REQUEST, "unknown mode")),
    }
}

// ── GET /api/session/root ────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct RootQuery {
    session: Option<String>,
}

pub async fn session_root(State(app): State<AppState>, Query(q): Query<RootQuery>) -> Json<Value> {
    let home = app.inner.home.to_string_lossy().to_string();
    let root = q
        .session
        .as_deref()
        .and_then(|s| app.get(s))
        .map(|s| s.live_cwd())
        .unwrap_or_else(|| home.clone());
    Json(json!({ "root": root, "home": home }))
}

// ── POST /api/key ────────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct KeyReq {
    session: String,
    key: String,
}

pub async fn key(State(app): State<AppState>, Json(req): Json<KeyReq>) -> ApiResult {
    let sess = app
        .get(&req.session)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown session"))?;
    let bytes = key_bytes(&req.key)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("unknown key: {}", req.key)))?;
    sess.write_input(bytes);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── POST /api/paste ──────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct PasteReq {
    session: String,
    text: String,
    #[serde(default)]
    enter: bool,
}

pub async fn paste(State(app): State<AppState>, Json(req): Json<PasteReq>) -> ApiResult {
    let sess = app
        .get(&req.session)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown session"))?;
    // Bracketed paste so newlines don't submit line-by-line inside a TUI.
    let mut buf = Vec::with_capacity(req.text.len() + 16);
    buf.extend_from_slice(b"\x1b[200~");
    buf.extend_from_slice(req.text.as_bytes());
    buf.extend_from_slice(b"\x1b[201~");
    if req.enter {
        buf.extend_from_slice(b"\r");
    }
    sess.write_input(&buf);
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── POST /api/clear-history ──────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct ClearReq {
    session: String,
}

pub async fn clear_history(State(app): State<AppState>, Json(req): Json<ClearReq>) -> ApiResult {
    let sess = app
        .get(&req.session)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown session"))?;
    sess.clear_history();
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── GET /api/sessions/restorable ─────────────────────────────────────────────
pub async fn restorable(State(app): State<AppState>) -> Json<Value> {
    let list: Vec<Value> = app
        .restorable()
        .into_iter()
        .map(|e| json!({ "session": e.session, "tool": e.prefix, "short": e.short, "dir": e.dir }))
        .collect();
    Json(json!(list))
}

// ── POST /api/sessions/restore ───────────────────────────────────────────────
pub async fn restore(State(app): State<AppState>) -> Json<Value> {
    let (restored, failed) = app.restore_all();
    let mut out = json!({ "restored": restored });
    if !failed.is_empty() {
        out["failed"] = json!(failed);
    }
    Json(out)
}

// ── GET/PUT /api/favorites ───────────────────────────────────────────────────
#[derive(Serialize, Deserialize, Clone)]
pub struct Favorite {
    id: String,
    text: String,
}

fn favorites_path(app: &AppState) -> PathBuf {
    app.inner.config_dir.join("favorites.json")
}

pub async fn get_favorites(State(app): State<AppState>) -> Json<Vec<Favorite>> {
    let list = std::fs::read_to_string(favorites_path(&app))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Favorite>>(&s).ok())
        .unwrap_or_default();
    Json(list)
}

pub async fn put_favorites(State(app): State<AppState>, Json(list): Json<Vec<Favorite>>) -> ApiResult {
    const MAX_FAV: usize = 200;
    const MAX_FAV_LEN: usize = 8000;
    let mut clean: Vec<Favorite> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for f in list {
        let text = f.text.trim();
        let id = f.id.trim().to_string();
        // Drop blanks / id-less / duplicate-id entries, matching the Go store.
        if text.is_empty() || id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let text: String = text.chars().take(MAX_FAV_LEN).collect();
        clean.push(Favorite { id, text });
        if clean.len() >= MAX_FAV {
            break;
        }
    }
    let path = favorites_path(&app);
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(&clean).unwrap_or_default();
    std::fs::write(&tmp, &body)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(clean).into_response())
}

// ── GET /api/ws ──────────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct WsQuery {
    session: String,
}

pub async fn ws(
    State(app): State<AppState>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    match app.get(&q.session) {
        Some(sess) => ws.on_upgrade(move |socket| handle_socket(socket, sess)),
        None => (StatusCode::NOT_FOUND, "unknown session").into_response(),
    }
}

fn handle_frame(sess: &Session, text: &str) {
    #[derive(Deserialize)]
    struct M {
        t: String,
        #[serde(default)]
        d: String,
        #[serde(default)]
        c: u16,
        #[serde(default)]
        r: u16,
    }
    if let Ok(m) = serde_json::from_str::<M>(text) {
        match m.t.as_str() {
            "i" => sess.write_input(m.d.as_bytes()),
            "r" => sess.resize(m.c, m.r),
            _ => {} // "s" (scroll) is client-side now — see TerminalView patch
        }
    }
}

async fn handle_socket(socket: WebSocket, sess: Arc<Session>) {
    let (mut sink, mut stream) = socket.split();
    // Atomic snapshot + subscribe (see Session::attach).
    let (snap, mut rx) = sess.attach();
    let mut closed_rx = sess.closed_rx();

    let sess_send = sess.clone();
    let mut send_task = tokio::spawn(async move {
        if sink.send(Message::Binary(snap)).await.is_err() {
            return;
        }
        // Keepalive so phone NAT/proxies don't reap an idle socket.
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
        ping.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(b) => {
                        if sink.send(Message::Binary(b.to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // A slow client fell behind the ring; resync with a fresh snapshot.
                        if sink.send(Message::Binary(sess_send.snapshot())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
                // The child exited — close the socket now so the client stops
                // showing a frozen frame and re-polls (the session is gone).
                _ = closed_rx.changed() => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    let sess_recv = sess.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(t) => handle_frame(&sess_recv, &t),
                Message::Binary(b) => sess_recv.write_input(&b),
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }
}

// ── Static frontend (embedded) ───────────────────────────────────────────────
fn content_type(path: &str) -> String {
    if path.ends_with(".mjs") {
        // pdf.js's module worker; strict module MIME rejects octet-stream.
        return "text/javascript".to_string();
    }
    if path.ends_with(".webmanifest") {
        return "application/manifest+json".to_string();
    }
    mime_guess::from_path(path).first_or_octet_stream().to_string()
}

pub async fn static_handler(uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let path = if raw.is_empty() { "index.html" } else { raw };
    if let Some(f) = Assets::get(path) {
        let ct = content_type(path);
        return ([(header::CONTENT_TYPE, ct)], Bytes::from(f.data.into_owned())).into_response();
    }
    // SPA fallback so client routing works on a hard refresh.
    if let Some(f) = Assets::get("index.html") {
        return (
            [(header::CONTENT_TYPE, "text/html".to_string())],
            Bytes::from(f.data.into_owned()),
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "frontend not built").into_response()
}
