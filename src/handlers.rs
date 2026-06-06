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
        Query, RawQuery, State,
    },
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
#[cfg(feature = "frontend")]
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use cc_screen_protocol::{
    key_bytes, wrap_bracketed_paste, AuthStatus, CreateReq, CreateResp, DeleteReq, Favorite,
    LoginReq, RestorableSession, SessionInfo, ToolInfo, WsClientFrame,
};

use crate::confine::resolve_under;
use crate::engine::{AppState, Session};
use crate::tools::Tool;

// The React PWA, embedded at build time. Gated behind the default-on `frontend`
// feature so a fleet agent that only talks to a hub can build headless
// (`--no-default-features`) without `frontend/dist` or the rust-embed compile.
#[cfg(feature = "frontend")]
#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist"]
struct Assets;

type ApiResult = Result<Response, (StatusCode, String)>;

fn err(code: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (code, msg.into())
}

// ── Auth (opt-in; see auth.rs) ────────────────────────────────────────────────
// These three are exempt from the auth middleware so the login flow can run
// before a client is authenticated.

// POST /api/login — `{secret}` matching the password OR the API token mints the
// 2-week session cookie. A wrong secret gets a fixed delay to blunt guessing.
pub async fn login(
    State(app): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    let auth = &app.inner.auth;
    if auth.verify_login(&req.secret) {
        let cookie = auth.issue_cookie(crate::auth::is_https(&headers));
        return (StatusCode::OK, [(header::SET_COOKIE, cookie)], Json(json!({ "ok": true })))
            .into_response();
    }
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false }))).into_response()
}

// GET /api/auth — the frontend gates itself on this at boot (and the middleware
// 401s here if the cookie later expires). `authed` is always true when the gate
// is off.
pub async fn auth_status(
    State(app): State<AppState>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Json<AuthStatus> {
    let auth = &app.inner.auth;
    Json(AuthStatus {
        auth_required: auth.enabled(),
        authed: !auth.enabled() || auth.is_authed(&headers, q.as_deref()),
    })
}

// POST /api/logout — clear the session cookie.
pub async fn logout(State(app): State<AppState>) -> Response {
    let cookie = app.inner.auth.clear_cookie();
    (StatusCode::OK, [(header::SET_COOKIE, cookie)]).into_response()
}

// ── GET /api/sessions ────────────────────────────────────────────────────────
/// Build the live session list. Shared by the `/api/sessions` handler and the
/// hub uplink (`src/uplink.rs`), so both report sessions identically. `machine`
/// is left empty: a standalone agent doesn't know its own hub-assigned name, and
/// the hub stamps it when aggregating (empty is omitted on the wire, so the
/// single-machine UI is unchanged).
pub fn session_list(app: &AppState) -> Vec<SessionInfo> {
    app.list()
        .into_iter()
        .map(|s| SessionInfo {
            name: s.name.clone(),
            tool: s.tool.clone(),
            short: s.short.clone(),
            attached: s.attached(),
            activity: s.last_activity() as i64,
            last_input_at: s.last_input_at(),
            busy_since: s.busy_since(),
            preview: s.preview(),
            waiting: s.waiting(),
            cwd: s.live_cwd(),
            machine: String::new(),
        })
        .collect()
}

pub async fn sessions(State(app): State<AppState>) -> Json<Vec<SessionInfo>> {
    Json(session_list(&app))
}

// ── GET /api/tools ───────────────────────────────────────────────────────────
pub async fn tools(State(app): State<AppState>) -> Json<Vec<ToolInfo>> {
    let list: Vec<ToolInfo> = app.inner.tools.iter().map(crate::tools::tool_info).collect();
    Json(list)
}

// ── POST /api/session ────────────────────────────────────────────────────────
/// Validate + create a session. Shared by the `/api/session` handler and the hub
/// `Cmd::Create` dispatch (`crate::ops`), so a hub-routed create runs the exact
/// same confinement + validation as a direct one.
pub fn create_core(app: &AppState, req: &CreateReq) -> Result<String, (StatusCode, String)> {
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
    let extra = validate_extra_dirs(app, &tool, &dir, &req.extra_dirs)?;
    // Empty name defaults to the dir's basename (mirrors the Go build).
    let mut name = req.name.trim().to_string();
    if name.is_empty() {
        name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    }
    app.create(&tool, &name, &dir.to_string_lossy(), extra, false).map_err(|e| {
        let msg = e.to_string();
        let code =
            if msg.contains("already exists") { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST };
        (code, msg)
    })
}

pub async fn create_session(State(app): State<AppState>, Json(req): Json<CreateReq>) -> ApiResult {
    match create_core(&app, &req) {
        Ok(full) => Ok(Json(CreateResp { name: full }).into_response()),
        Err(e) => Err(e),
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
    // `machine` lets a direct client (no hub) name this box in its UI. In hub
    // mode the client uses `/api/machines` instead (the hub's relay of this route
    // doesn't carry it), so the field is harmless there.
    Json(json!({ "root": root, "home": home, "machine": app.inner.machine_id }))
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
    let buf = wrap_bracketed_paste(&req.text, req.enter);
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
pub async fn restorable(State(app): State<AppState>) -> Json<Vec<RestorableSession>> {
    let list: Vec<RestorableSession> = app
        .restorable()
        .into_iter()
        .map(|e| RestorableSession {
            session: e.session,
            tool: e.prefix,
            short: e.short,
            dir: e.dir,
        })
        .collect();
    Json(list)
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

// ── Web Push ("agent finished" phone notifications) ──────────────────────────
// GET /api/push/key returns the VAPID public (application server) key the client
// subscribes with. POST subscribe/unsubscribe manage the device list; POST test
// fires a buzz on demand so the user can confirm the wiring on their phone.

pub async fn push_key(State(app): State<AppState>) -> Json<Value> {
    Json(json!({ "key": app.inner.push.application_server_key() }))
}

/// The browser's `PushSubscription` shape (the bits we need).
#[derive(Deserialize)]
pub struct SubscribeReq {
    endpoint: String,
    keys: SubKeys,
}
#[derive(Deserialize)]
pub struct SubKeys {
    p256dh: String,
    auth: String,
}

pub async fn push_subscribe(
    State(app): State<AppState>,
    Json(req): Json<SubscribeReq>,
) -> ApiResult {
    if req.endpoint.is_empty() || req.keys.p256dh.is_empty() || req.keys.auth.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "incomplete subscription"));
    }
    app.inner.push.add_sub(crate::push::StoredSub {
        endpoint: req.endpoint,
        p256dh: req.keys.p256dh,
        auth: req.keys.auth,
    });
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct UnsubscribeReq {
    endpoint: String,
}

pub async fn push_unsubscribe(
    State(app): State<AppState>,
    Json(req): Json<UnsubscribeReq>,
) -> ApiResult {
    app.inner.push.remove_sub(&req.endpoint);
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn push_test(State(app): State<AppState>) -> ApiResult {
    app.inner
        .push
        .notify("cc-screen", "🔔 Test buzz — notifications are on", "")
        .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ── GET /api/ws ──────────────────────────────────────────────────────────────
#[derive(Deserialize)]
pub struct WsQuery {
    session: String,
}

pub async fn ws(
    State(app): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Defense-in-depth: the auth middleware already ran the Origin/Host check, but
    // the terminal WS is the RCE path — re-check before upgrading.
    if !app.inner.origin.check(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
    }
    match app.get(&q.session) {
        Some(sess) => ws.on_upgrade(move |socket| handle_socket(socket, sess)),
        None => (StatusCode::NOT_FOUND, "unknown session").into_response(),
    }
}

async fn handle_socket(socket: WebSocket, sess: Arc<Session>) {
    use crate::attach::{attach_loop, AttachOut, ClientEvent};

    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<AttachOut>(256);
    let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<ClientEvent>(256);

    // Transport writer: engine→client frames + a 30s keepalive ping so phone
    // NAT/proxies don't reap an idle socket.
    let send_task = tokio::spawn(async move {
        let mut ping = tokio::time::interval(std::time::Duration::from_secs(30));
        ping.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                o = out_rx.recv() => match o {
                    Some(AttachOut::Snapshot(b)) | Some(AttachOut::Output(b)) => {
                        if sink.send(Message::Binary(b)).await.is_err() {
                            break;
                        }
                    }
                    // The child exited — close the socket so the client stops
                    // showing a frozen frame and re-polls (the session is gone).
                    Some(AttachOut::Closed) => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Transport reader: client→engine events. Input is a {t:"i"} text frame or a
    // raw binary frame; {t:"r"} is a resize. ("s"/scroll is client-side now.)
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let ev = match msg {
                Message::Text(t) => match serde_json::from_str::<WsClientFrame>(&t) {
                    Ok(m) => match m.t.as_str() {
                        "i" => Some(ClientEvent::Input(m.d.into_bytes())),
                        "r" => Some(ClientEvent::Resize(m.c, m.r)),
                        _ => None,
                    },
                    Err(_) => None,
                },
                Message::Binary(b) => Some(ClientEvent::Input(b)),
                Message::Close(_) => break,
                _ => None,
            };
            if let Some(ev) = ev {
                if ev_tx.send(ev).await.is_err() {
                    break;
                }
            }
        }
    });

    // Drive the engine here; attach_loop ALWAYS unregisters on exit (releasing the
    // PTY min-size pin). When it returns, out_tx drops → send_task drains its final
    // frame (incl. a Close) and ends; the reader is blocked on the socket, abort it.
    attach_loop(sess, out_tx, ev_rx).await;
    recv_task.abort();
    let _ = send_task.await;
}

// ── Static frontend (embedded) ───────────────────────────────────────────────
#[cfg(feature = "frontend")]
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

#[cfg(feature = "frontend")]
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

// Headless build: no embedded PWA. The agent serves only the API; reach its UI
// through the hub (which embeds the frontend) or run a non-headless agent.
#[cfg(not(feature = "frontend"))]
pub async fn static_handler(_uri: Uri) -> Response {
    (StatusCode::NOT_FOUND, "frontend not embedded (headless agent build)").into_response()
}
