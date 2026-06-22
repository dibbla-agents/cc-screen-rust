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
    /// Last client input timestamp (Unix seconds). Additive/defaulted so older
    /// agents parse as 0; used by notification gating and future UI affordances.
    #[serde(default)]
    pub last_input_at: u64,
    /// When the current/last turn began (Unix seconds) — the time of the user
    /// *submit* that armed it (proposal 0024). 0 = never submitted to. Used as the
    /// "working for N" timer anchor and the notification work-duration gate.
    #[serde(default)]
    pub busy_since: u64,
    /// The busy-window deadline (Unix seconds). While the session is **working**
    /// this is in the future (it's extended by output); once the session is
    /// **ready** it sits in the past and equals the instant it transitioned
    /// busy→ready — and, unlike `activity`, it is NOT bumped by cosmetic repaints
    /// (focus/resize), so the ready surfaces anchor their "ready for N" timer and
    /// sort to it for a stable count that doesn't reset when you focus a session
    /// (proposal 0024). 0 = never armed → clients fall back to `activity`.
    #[serde(default)]
    pub busy_until: u64,
    pub preview: String,
    /// True when the session is ready / "your turn": it is **not** in an open,
    /// submit-armed busy window. Under the input-gated model (proposal 0024) a
    /// session reads ready until a user submit (Enter) arms it, stays `false`
    /// (working) while the agent's output sustains the window, and flips back to
    /// `true` a grace window after output goes quiet — so cosmetic repaints
    /// (focus/resize/spinner) never make it read busy. The server computes this;
    /// see the server's `WORK_GRACE_SECS`. `#[serde(default)]` so a TUI talking to
    /// an older server still parses.
    #[serde(default)]
    pub waiting: bool,
    /// Whether the session launched in YOLO mode (its approval-bypass flag).
    /// Informational — drives a "YOLO"/"safe" badge. `None` = unknown (pre-0005
    /// agent). Omitted on the wire when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_permissions: Option<bool>,
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
    /// LLM-summarized headline (≤ 6 words): what this session is doing / needs,
    /// at a glance. Produced by the hub's Haiku call (proposal 0022) and cached on
    /// the agent; `None` until computed / when the feature is off, in which case
    /// clients fall back to `preview`. Additive + omitted-when-absent, so older
    /// clients and feature-off agents are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headline: Option<String>,
    /// LLM-summarized detail (2–3 sentences) — the tooltip / status-view / push
    /// body. Same lifecycle + back-compat as `headline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Operator-chosen accent colour for this session (proposal 0029): a short
    /// palette token (e.g. "rose"/"teal"), NOT a raw colour — the client owns the
    /// rendered shade. `None`/absent = unmarked. Additive + omitted-when-absent,
    /// so older clients and feature-off agents are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Operator-chosen display label for this session (proposal 0035): a free-text
    /// name shown *in place of* `short` wherever the session is named. Display-only
    /// — it never replaces the identity `name`/`short`, so routing/persistence keys
    /// are untouched. `None`/absent = no label, fall back to `short`. Additive +
    /// omitted-when-absent, so older clients and feature-off agents are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The curated per-session mark palette tokens (proposal 0029). The *rendered*
/// shade is owned by the client (`frontend/src/util.ts` `SESSION_COLORS`); the
/// wire only carries these stable ids, so an old client renders an unknown token
/// as unmarked rather than a broken colour. The agent validates a `SetColor`
/// against this set — **keep it in lockstep with `util.ts`**. The set deliberately
/// excludes hues near the reserved status colours (cyan/amber/green/red).
pub const SESSION_COLOR_TOKENS: &[&str] =
    &["rose", "magenta", "violet", "indigo", "teal", "lime", "orange", "slate"];

/// Whether `token` is a known session-mark colour (proposal 0029).
pub fn is_valid_color_token(token: &str) -> bool {
    SESSION_COLOR_TOKENS.contains(&token)
}

/// Max display-label length in chars (proposal 0035). Display-only, so generous
/// but bounded — sized to the identity bar / switcher row before truncation.
pub const MAX_SESSION_LABEL_LEN: usize = 60;

/// Trim + length-check a proposed display label (proposal 0035). Returns the
/// normalized label, or `None` to clear (empty after trim). `Err` if it exceeds
/// the cap. Unlike a session slug, the label is display-only and never becomes a
/// process/filesystem name, so it is passed through verbatim (spaces, capitals,
/// punctuation, emoji allowed) — no `sanitize_name` rules.
pub fn normalize_session_label(raw: &str) -> Result<Option<String>, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if t.chars().count() > MAX_SESSION_LABEL_LEN {
        return Err(format!("label too long (max {MAX_SESSION_LABEL_LEN})"));
    }
    Ok(Some(t.to_string()))
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReq {
    pub tool: String,
    pub name: String,
    pub dir: String,
    #[serde(default)]
    pub extra_dirs: Vec<String>,
    /// Launch the CLI in YOLO mode (`--dangerously-skip-permissions` / `-y` /
    /// `--dangerously-bypass-approvals-and-sandbox`). Defaults to **true** so an
    /// older client (which omits it) reproduces today's behavior. See 0005.
    #[serde(default = "default_true")]
    pub skip_permissions: bool,
}

fn default_true() -> bool {
    true
}

// Hand-written so `CreateReq::default()` matches the documented + serde default
// (skip-permissions on) rather than `bool::default()` (false). (0014 retired the
// `remote_control` switch; a stray `remoteControl` on the wire is ignored.)
impl Default for CreateReq {
    fn default() -> Self {
        Self {
            tool: String::new(),
            name: String::new(),
            dir: String::new(),
            extra_dirs: Vec::new(),
            skip_permissions: true,
        }
    }
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
            last_input_at: 0,
            busy_since: 0,
            busy_until: 0,
            preview: "p".into(),
            waiting: false,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
            label: None,
        };
        let v = serde_json::to_string(&s).unwrap();
        assert!(!v.contains("cwd"), "empty cwd should be omitted: {v}");
        assert!(!v.contains("headline"), "absent headline should be omitted: {v}");
        assert!(!v.contains("detail"), "absent detail should be omitted: {v}");
        assert!(!v.contains("machine"), "empty machine should be omitted: {v}");
        assert!(!v.contains("remote_control"), "retired policy must never serialize: {v}");
        assert!(!v.contains("skip_permissions"), "unknown policy should be omitted: {v}");
        assert!(!v.contains("color"), "absent color should be omitted: {v}");
        assert!(!v.contains("label"), "absent label should be omitted: {v}");
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
            last_input_at: 111,
            busy_since: 222,
            busy_until: 333,
            preview: String::new(),
            waiting: false,
            skip_permissions: Some(true),
            cwd: String::new(),
            machine: "laptop".into(),
            headline: Some("Waiting to run tests".into()),
            detail: Some("It refactored auth and is paused.".into()),
            color: Some("teal".into()),
            label: Some("Auth refactor".into()),
        };
        let v = serde_json::to_string(&s).unwrap();
        assert!(v.contains(r#""machine":"laptop""#), "machine should serialize when set: {v}");
        assert!(v.contains(r#""color":"teal""#), "color should serialize when set: {v}");
        assert!(v.contains(r#""label":"Auth refactor""#), "label should serialize when set: {v}");
        assert!(v.contains(r#""headline":"Waiting to run tests""#), "headline serializes when set: {v}");
        assert!(v.contains(r#""skip_permissions":true"#), "yolo should serialize when set: {v}");

        // A NEW client parsing an OLD payload (no `machine`, and a now-retired
        // `remoteControl`) reads `machine` as "" and ignores the stray field.
        let old: SessionInfo = serde_json::from_str(
            r#"{"name":"claude-x","tool":"claude","short":"x","attached":false,"activity":0,"preview":"p","waiting":false,"remote_control":false}"#,
        )
        .unwrap();
        assert_eq!(old.machine, "");
        assert_eq!(old.last_input_at, 0);
        assert_eq!(old.busy_since, 0);
        assert_eq!(old.busy_until, 0, "old payload → no busy_until, falls back to activity");
        assert_eq!(old.skip_permissions, None);
        assert_eq!(old.headline, None, "old payload → no summary");
        assert_eq!(old.detail, None);
        assert_eq!(old.color, None, "old payload → no mark colour");
        assert_eq!(old.label, None, "old payload → no display label");

        // An OLD client parsing a NEW payload (with `machine`) ignores nothing it
        // needs — the rest still parses (forward-compat); round-trip the value.
        let back: SessionInfo = serde_json::from_str(&v).unwrap();
        assert_eq!(back.machine, "laptop");
        assert_eq!(back.last_input_at, 111);
        assert_eq!(back.busy_since, 222);
        assert_eq!(back.busy_until, 333);
        assert_eq!(back.color, Some("teal".into()));
        assert_eq!(back.label, Some("Auth refactor".into()));
    }

    #[test]
    fn normalize_session_label_trims_clears_and_caps() {
        // Trim surrounding whitespace, keep inner spacing/case verbatim.
        assert_eq!(
            normalize_session_label("  My Session  ").unwrap(),
            Some("My Session".into())
        );
        // Empty (or whitespace-only) → clear.
        assert_eq!(normalize_session_label("").unwrap(), None);
        assert_eq!(normalize_session_label("   ").unwrap(), None);
        // Exactly at the cap is fine; over the cap is rejected.
        let at_cap: String = "a".repeat(MAX_SESSION_LABEL_LEN);
        assert_eq!(normalize_session_label(&at_cap).unwrap(), Some(at_cap.clone()));
        let over_cap: String = "a".repeat(MAX_SESSION_LABEL_LEN + 1);
        assert!(normalize_session_label(&over_cap).is_err());
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
    fn create_req_skip_permissions_default_and_ignores_remote_control() {
        // skip-permissions still defaults to true when omitted (untouched by 0014).
        let r: CreateReq =
            serde_json::from_str(r#"{"tool":"cc","name":"x","dir":"/tmp"}"#).unwrap();
        assert!(r.skip_permissions, "missing skipPermissions must default to true");
        assert!(r.extra_dirs.is_empty());
        // A stray `remoteControl` from an older client still deserializes — the
        // retired field is ignored (serde unknown-field tolerance), so the create
        // succeeds and the session is editable (0014).
        let legacy: CreateReq = serde_json::from_str(
            r#"{"tool":"cc","name":"x","dir":"/tmp","skipPermissions":false,"remoteControl":false}"#,
        )
        .unwrap();
        assert!(!legacy.skip_permissions, "skipPermissions still round-trips");
        // The hand-written Default matches the serde default.
        let d = CreateReq::default();
        assert!(d.skip_permissions);
        // camelCase on the wire, matching the React PWA — and no remoteControl key.
        let s = serde_json::to_string(&CreateReq {
            tool: "cc".into(),
            name: "x".into(),
            dir: "/tmp".into(),
            extra_dirs: vec![],
            skip_permissions: false,
        })
        .unwrap();
        assert!(s.contains(r#""skipPermissions":false"#), "{s}");
        assert!(!s.contains("remoteControl"), "retired field must not serialize: {s}");
    }

    #[test]
    fn bracketed_paste_wraps() {
        assert_eq!(wrap_bracketed_paste("hi", false), b"\x1b[200~hi\x1b[201~");
        assert_eq!(wrap_bracketed_paste("hi", true), b"\x1b[200~hi\x1b[201~\r");
    }
}
