//! Shared wire types for cc-screen-rust.
//!
//! Both the server (`src/`) and the terminal client (`crates/tui`) speak the
//! same HTTP+WebSocket contract; keeping the DTOs here is the single source of
//! truth that stops the two sides from drifting. The server *serializes* these
//! on its responses and *deserializes* the request shapes; the TUI does the
//! mirror. The JSON field names match what the React PWA already expects.

use serde::{Deserialize, Serialize};

/// The agent↔hub envelope (multiplexed control + terminal/file streams). Behind
/// the `hub` feature so a standalone TUI build doesn't compile it.
#[cfg(feature = "hub")]
pub mod hub;

/// Where the `curl | sh` installers are hosted (the Dibbla docs site, off our own
/// domain — see README "Install"). The `update` subcommand on each binary re-runs
/// its installer from here: `<RELEASE_BASE_URL>/install-<name>.sh`. Single source
/// of truth shared by the agent, the hub, and the TUI.
pub const RELEASE_BASE_URL: &str = "https://cc-screen-b4687da9.dibbla.app/dl";

// ── GET /api/sessions ────────────────────────────────────────────────────────
/// One entry in the session list. (Named `SessionInfo`, not `Session`, so it
/// doesn't collide with the server's live `engine::Session`.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub tool: String,
    pub short: String,
    pub attached: bool,
    pub activity: i64,
    pub preview: String,
    /// True when the agent has produced no output for a few seconds — it has
    /// stopped streaming and is (almost always) waiting for input. `false` while
    /// it's actively working (the CLIs animate a sub-second spinner). The server
    /// computes this from `activity`; see the server's `IDLE_AFTER_SECS`.
    /// `#[serde(default)]` so a TUI talking to an older server still parses.
    #[serde(default)]
    pub waiting: bool,
    /// The session's live cwd; omitted when empty (the server can't read it).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// The machine (agent) this session lives on. Stamped by the hub when it
    /// aggregates several agents; empty for a single agent talking to a client
    /// directly — then it's omitted on the wire, so an older client still parses
    /// it and the single-machine UI is unchanged. `#[serde(default)]` so a client
    /// talking to a hub-less server reads it as "".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub machine: String,
}

// ── GET /api/tools ───────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub cmd: String,
    pub prefix: String,
    /// Present only for tools that accept extra workspace dirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_dirs: Option<ExtraDirs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraDirs {
    /// Max extra dirs (omitted when unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

// ── GET /api/sessions/restorable ─────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestorableSession {
    pub session: String,
    pub tool: String,
    pub short: String,
    pub dir: String,
}

// ── GET/PUT /api/favorites ───────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Favorite {
    pub id: String,
    pub text: String,
}

// ── Auth (opt-in password / API-token gate) ──────────────────────────────────
/// `POST /api/login` body. `secret` is the password *or* the API token — the
/// web login accepts either; a match mints the 2-week session cookie.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoginReq {
    pub secret: String,
}

/// `GET /api/auth` reply. The frontend gates itself on this at boot:
/// `authRequired=false` → no login screen; else show it unless already `authed`
/// (via a valid cookie or token).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub auth_required: bool,
    pub authed: bool,
}

// ── POST /api/session ────────────────────────────────────────────────────────
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReq {
    pub tool: String,
    pub name: String,
    pub dir: String,
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateResp {
    pub name: String,
}

// ── GET /api/machines (hub only) ─────────────────────────────────────────────
/// One connected agent, as returned by the hub's `/api/machines` (absent on a
/// standalone agent — clients treat a 404 there as "single, unnamed machine").
/// Enough for the machine picker + offline greying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub machine: String,
    pub hostname: String,
    pub online: bool,
}

// ── POST /api/session/delete ─────────────────────────────────────────────────
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeleteReq {
    pub session: String,
    /// "exit" (graceful) | "kill" (hard); empty defaults to kill server-side.
    #[serde(default)]
    pub mode: String,
}

// ── WebSocket client → server frame (GET /api/ws) ────────────────────────────
/// The `{t,d,c,r}` frame the client sends over the terminal WebSocket. `t="i"`
/// carries input bytes in `d`; `t="r"` carries a resize in `c`/`r`. (Input can
/// also be sent as a raw *binary* WS frame — the server writes those straight to
/// the PTY — which the TUI prefers so non-UTF-8 byte sequences need no escaping.
/// Resize must use this text frame.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WsClientFrame {
    pub t: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub d: String,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub c: u16,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub r: u16,
}

fn is_zero_u16(n: &u16) -> bool {
    *n == 0
}

impl WsClientFrame {
    pub fn input(d: impl Into<String>) -> Self {
        Self { t: "i".into(), d: d.into(), ..Default::default() }
    }
    pub fn resize(cols: u16, rows: u16) -> Self {
        Self { t: "r".into(), c: cols, r: rows, ..Default::default() }
    }
}

// ── Terminal byte-stream constants ───────────────────────────────────────────
/// RIS (full terminal reset). The server prefixes every (re)attach snapshot with
/// this so a fresh emulator repaints cleanly before the replayed history.
pub const SNAPSHOT_RESET: &[u8] = b"\x1bc";
/// Bracketed-paste start/end — wrap pasted text so a TUI treats newlines as data
/// rather than submitting line-by-line.
pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Wrap `text` in a bracketed-paste sequence, optionally appending Enter.
pub fn wrap_bracketed_paste(text: &str, enter: bool) -> Vec<u8> {
    let mut buf = Vec::with_capacity(text.len() + PASTE_START.len() + PASTE_END.len() + 1);
    buf.extend_from_slice(PASTE_START);
    buf.extend_from_slice(text.as_bytes());
    buf.extend_from_slice(PASTE_END);
    if enter {
        buf.push(b'\r');
    }
    buf
}

// ── Named key → byte sequence (the /api/key allow-list) ──────────────────────
/// Maps a named key to the bytes to inject. Used by the server's `/api/key`
/// endpoint; the TUI shares the table where it sends named keys rather than raw
/// input. Names are lowercased before lookup.
pub fn key_bytes(name: &str) -> Option<&'static [u8]> {
    let b: &'static [u8] = match name.to_ascii_lowercase().as_str() {
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "enter" => b"\r",
        "escape" | "esc" => b"\x1b",
        "tab" => b"\t",
        "btab" => b"\x1b[Z",
        "space" => b" ",
        "backspace" => b"\x7f",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "c-c" => b"\x03",
        "c-d" => b"\x04",
        "c-z" => b"\x1a",
        "c-l" => b"\x0c",
        "c-r" => b"\x12",
        _ => return None,
    };
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_bytes_lookup() {
        assert_eq!(key_bytes("up"), Some(&b"\x1b[A"[..]));
        assert_eq!(key_bytes("ENTER"), Some(&b"\r"[..])); // case-insensitive
        assert_eq!(key_bytes("c-c"), Some(&b"\x03"[..]));
        assert_eq!(key_bytes("nope"), None);
    }

    #[test]
    fn ws_frame_input_serializes_minimally() {
        let s = serde_json::to_string(&WsClientFrame::input("abc")).unwrap();
        assert_eq!(s, r#"{"t":"i","d":"abc"}"#); // no c/r
    }

    #[test]
    fn ws_frame_resize_serializes_minimally() {
        let s = serde_json::to_string(&WsClientFrame::resize(80, 24)).unwrap();
        assert_eq!(s, r#"{"t":"r","c":80,"r":24}"#); // no d
    }

    #[test]
    fn ws_frame_roundtrips_from_server_shape() {
        let f: WsClientFrame = serde_json::from_str(r#"{"t":"i","d":"x"}"#).unwrap();
        assert_eq!(f.t, "i");
        assert_eq!(f.d, "x");
        assert_eq!((f.c, f.r), (0, 0));
    }

    #[test]
    fn tool_info_json_parity() {
        // No extra-dir support → no extraDirs key.
        let t = ToolInfo { cmd: "cc".into(), prefix: "claude".into(), extra_dirs: None };
        assert_eq!(serde_json::to_string(&t).unwrap(), r#"{"cmd":"cc","prefix":"claude"}"#);
        // Supports extra dirs, unlimited → empty object.
        let t = ToolInfo {
            cmd: "gm".into(),
            prefix: "gemini".into(),
            extra_dirs: Some(ExtraDirs { max: None }),
        };
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"cmd":"gm","prefix":"gemini","extraDirs":{}}"#
        );
        // Bounded → {"max":N}.
        let t = ToolInfo {
            cmd: "cx".into(),
            prefix: "codex".into(),
            extra_dirs: Some(ExtraDirs { max: Some(8) }),
        };
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"cmd":"cx","prefix":"codex","extraDirs":{"max":8}}"#
        );
    }

    #[test]
    fn session_info_omits_empty_cwd() {
        let s = SessionInfo {
            name: "claude-x".into(),
            tool: "claude".into(),
            short: "x".into(),
            attached: false,
            activity: 0,
            preview: "p".into(),
            waiting: false,
            cwd: String::new(),
            machine: String::new(),
        };
        let v = serde_json::to_string(&s).unwrap();
        assert!(!v.contains("cwd"), "empty cwd should be omitted: {v}");
        assert!(!v.contains("machine"), "empty machine should be omitted: {v}");
        assert!(v.contains(r#""waiting":false"#), "waiting should always serialize: {v}");
    }

    #[test]
    fn session_info_machine_roundtrips_and_is_back_compat() {
        // Present when set (hub-aggregated): key appears.
        let s = SessionInfo {
            name: "claude-x".into(),
            tool: "claude".into(),
            short: "x".into(),
            attached: false,
            activity: 0,
            preview: String::new(),
            waiting: false,
            cwd: String::new(),
            machine: "laptop".into(),
        };
        let v = serde_json::to_string(&s).unwrap();
        assert!(v.contains(r#""machine":"laptop""#), "machine should serialize when set: {v}");

        // A NEW client parsing an OLD payload (no `machine`) reads it as "".
        let old: SessionInfo = serde_json::from_str(
            r#"{"name":"claude-x","tool":"claude","short":"x","attached":false,"activity":0,"preview":"p","waiting":false}"#,
        )
        .unwrap();
        assert_eq!(old.machine, "");

        // An OLD client parsing a NEW payload (with `machine`) ignores nothing it
        // needs — the rest still parses (forward-compat); round-trip the value.
        let back: SessionInfo = serde_json::from_str(&v).unwrap();
        assert_eq!(back.machine, "laptop");
    }

    #[test]
    fn auth_types_json_parity() {
        // Login request the React PWA POSTs.
        let r: LoginReq = serde_json::from_str(r#"{"secret":"hunter2"}"#).unwrap();
        assert_eq!(r.secret, "hunter2");
        // Auth status the frontend gates on — camelCase keys.
        let s = AuthStatus { auth_required: true, authed: false };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"authRequired":true,"authed":false}"#
        );
    }

    #[test]
    fn bracketed_paste_wraps() {
        assert_eq!(wrap_bracketed_paste("hi", false), b"\x1b[200~hi\x1b[201~");
        assert_eq!(wrap_bracketed_paste("hi", true), b"\x1b[200~hi\x1b[201~\r");
    }
}
