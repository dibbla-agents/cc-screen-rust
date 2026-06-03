//! Agent-side dispatch for hub-routed control ops. Each [`Cmd`] runs the same
//! engine/handler logic a direct REST call would, and returns a [`CmdResult`] the
//! hub maps back to an HTTP response. Keeping it here (rather than in the uplink
//! loop) lets it be unit-tested and keeps the relay code thin.

use cc_screen_protocol::hub::{Cmd, CmdResult};
use cc_screen_protocol::{key_bytes, wrap_bracketed_paste, RestorableSession};
use serde_json::json;

use crate::engine::AppState;

/// 404-style result for an op naming a session that isn't live.
fn unknown_session() -> CmdResult {
    CmdResult::Error { code: 404, msg: "unknown session".into() }
}

pub fn run_cmd(app: &AppState, cmd: Cmd) -> CmdResult {
    match cmd {
        Cmd::Create(req) => match crate::handlers::create_core(app, &req) {
            Ok(name) => CmdResult::Created(name),
            Err((code, msg)) => CmdResult::Error { code: code.as_u16(), msg },
        },
        Cmd::Delete(req) => match app.get(&req.session) {
            Some(sess) => {
                // The user is ending it on purpose — forget it so a later restore
                // doesn't resurrect it (mirrors the REST delete handler).
                crate::manifest::forget(&app.inner.config_dir, &req.session);
                match req.mode.as_str() {
                    "exit" | "soft" => sess.graceful_exit(),
                    _ => sess.kill(),
                }
                CmdResult::Ok
            }
            None => unknown_session(),
        },
        Cmd::Key { session, key } => match app.get(&session) {
            Some(sess) => match key_bytes(&key) {
                Some(b) => {
                    sess.write_input(b);
                    CmdResult::Ok
                }
                None => CmdResult::Error { code: 400, msg: format!("unknown key: {key}") },
            },
            None => unknown_session(),
        },
        Cmd::Paste { session, text, enter } => match app.get(&session) {
            Some(sess) => {
                sess.write_input(&wrap_bracketed_paste(&text, enter));
                CmdResult::Ok
            }
            None => unknown_session(),
        },
        Cmd::ClearHistory { session } => match app.get(&session) {
            Some(sess) => {
                sess.clear_history();
                CmdResult::Ok
            }
            None => unknown_session(),
        },
        Cmd::Restorable => {
            let list = app
                .restorable()
                .into_iter()
                .map(|e| RestorableSession {
                    session: e.session,
                    tool: e.prefix,
                    short: e.short,
                    dir: e.dir,
                })
                .collect();
            CmdResult::Restorable(list)
        }
        Cmd::Restore => {
            let (restored, failed) = app.restore_all();
            CmdResult::Json(json!({ "restored": restored, "failed": failed }))
        }
        Cmd::SessionRoot { session } => {
            let home = app.inner.home.to_string_lossy().to_string();
            let root = session
                .as_deref()
                .and_then(|s| app.get(s))
                .map(|s| s.live_cwd())
                .unwrap_or_else(|| home.clone());
            CmdResult::SessionRoot { root, home }
        }
        // Favorites are hub-local (the hub keeps its own store); never routed.
        Cmd::GetFavorites | Cmd::PutFavorites(_) => {
            CmdResult::Error { code: 400, msg: "favorites are hub-local".into() }
        }
        Cmd::File { op, args } => crate::fileops::run(app, &op, args),
    }
}
