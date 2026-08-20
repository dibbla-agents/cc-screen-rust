//! The agent↔hub wire envelope.
//!
//! A **hub** aggregates many dev-machine **agents** (each agent is the
//! `cc-screen-rust` server). Agents dial *out* to the hub over a single
//! persistent WebSocket; clients (the PWA + `ccs`) talk to the hub, which
//! transparently multiplexes their terminal/file streams to the owning agent.
//!
//! This module is the contract on that uplink. It is kept here, next to the
//! client-facing wire types, so the two never drift — and it **reuses**
//! [`SessionInfo`], [`CreateReq`], [`ToolInfo`], … verbatim rather than redeclaring
//! them.
//!
//! ## Multiplexing
//!
//! One WebSocket per agent carries everything. **Channel 0 is control**
//! (`Register`/`Sessions`/`Reply`/`Command`/…); channels `1..` are per-attached
//! *client* streams — one [`ChannelId`] per browser/TUI connection, **allocated by
//! the hub**, so each maps 1:1 to a `register_client()` subscriber on the agent.
//!
//! ## Framing
//!
//! Each WebSocket *binary* message is exactly one frame:
//!
//! ```text
//! [u32 BE header_len][header: JSON of AgentMsg/HubMsg][payload: raw bytes]
//! ```
//!
//! The JSON header is small metadata; the raw payload tail carries PTY bytes
//! ([`AgentMsg::Output`]/[`HubMsg::Input`]/[`AgentMsg::Snapshot`]) and fs-watch
//! events ([`AgentMsg::WatchEvt`]) **without** base64 or a serde copy — terminal
//! output is the hot path and is binary, so it must never round-trip through
//! UTF-8. Frames whose message carries no payload have an empty tail.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{CreateReq, DeleteReq, Favorite, RestorableSession, SessionInfo, ToolInfo};

/// A logical stream inside one agent↔hub WebSocket. `0` is the control channel;
/// `1..` are per-attached-client terminal/watch streams (hub-allocated).
pub type ChannelId = u32;
/// Correlates a [`HubMsg::Command`] with its [`AgentMsg::Reply`].
pub type ReqId = u32;

/// Bumped on any breaking envelope change; sent in [`AgentMsg::Register`] so the
/// hub can refuse an incompatible agent rather than misframe its traffic.
pub const HUB_PROTO_VERSION: u16 = 1;

/// The oldest agent envelope version a current hub still speaks. The hub accepts
/// any agent whose `proto` is in `[MIN_SUPPORTED_PROTO, HUB_PROTO_VERSION]` and
/// operates at `min(agent, hub)` rather than demanding exact equality, so a
/// staggered fleet rollout keeps working in both directions — an old agent
/// against a new hub and a new agent against an old hub (proposal 0001 §9.3).
/// Prefer additive `#[serde(default)]` fields, which need no bump at all; raise
/// this only when an old frame shape becomes genuinely unsupportable.
pub const MIN_SUPPORTED_PROTO: u16 = 1;

/// The control channel id (`0`). Terminal/watch channels are `>= 1`.
pub const CONTROL_CHANNEL: ChannelId = 0;

/// Capability token: this agent understands [`Cmd::UpdateAssistants`] /
/// [`Cmd::UpdateStatus`] (proposal 0049). Advertised in [`AgentMsg::Register`]'s
/// `caps`; the hub refuses the op with `501` for an agent that doesn't list it.
pub const CAP_ASSISTANT_UPDATE: &str = "assistant-update";

/// Capability token: this agent can **install** the missing assistants for the
/// local user (proposal 0050) — i.e. it honours `install_missing` on
/// [`Cmd::UpdateAssistants`] and understands [`Cmd::InstallPlan`].
///
/// Load-bearing, because the field itself is additive: nothing in this crate uses
/// `deny_unknown_fields`, so a 0049-era agent would silently *ignore*
/// `install_missing` and run an update-only job while the user believed something
/// was installed. The token is what turns that silent downgrade into an honest
/// `501` from the hub.
pub const CAP_ASSISTANT_INSTALL: &str = "assistant-install";

/// Capability token: this agent understands [`Cmd::RestartSession`] — restarting
/// **one** named session and resuming its conversation (proposal 0087).
///
/// A token rather than a version sniff, for the reason [`CAP_ASSISTANT_UPDATE`]
/// spells out: an agent that predates the variant can't deserialize the `Cmd` at
/// all, so routing one would never be answered and the client would wait out the
/// relay's timeout into a `504`. With the token the hub refuses up front with an
/// honest `501`.
pub const CAP_SESSION_RESTART: &str = "session-restart";

/// WebSocket close code the hub uses to reject an agent uplink whose token is
/// **unauthorized** — unknown, or revoked because the machine was unlinked from
/// the dashboard. In the private-use range (4000–4999); echoes HTTP 401. The
/// agent distinguishes this from an ordinary disconnect so it stops hot-looping a
/// dead credential and tells the user to re-enroll (proposal 0048).
pub const UPLINK_CLOSE_UNAUTHORIZED: u16 = 4401;

// ── agent → hub ────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentMsg {
    /// First frame after the socket opens. Identifies the machine and advertises
    /// its tool registry; `proto` gates compatibility.
    Register {
        proto: u16,
        machine_id: String,
        hostname: String,
        agent_version: String,
        tools: Vec<ToolInfo>,
        /// Optional capability tokens this agent understands (see [`CAP_ASSISTANT_UPDATE`]).
        /// Absent on older agents — the hub then refuses the op with a clear `501`
        /// instead of routing a `Cmd` the agent can't deserialize and timing out
        /// into a `504`. Additive by design (proposal 0049): no proto bump.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        caps: Vec<String>,
    },
    /// The agent's live session list (sent on register and whenever it changes).
    /// `SessionInfo` is the same type the client-facing `/api/sessions` returns.
    Sessions { sessions: Vec<SessionInfo> },
    /// The agent's tool registry **re-probed** (proposal 0050): sent after an
    /// install job changes availability, so the hub's cached `unavailable` flags
    /// — and the dashboard badge that reads them — stop lying. Without this the
    /// list is frozen at `Register` time and a freshly installed CLI stays
    /// invisible until the agent reconnects.
    ///
    /// Safe against an **older hub** by inspection: an unknown `AgentMsg` variant
    /// fails deserialization there, and that arm logs "malformed frame (skipped)"
    /// and drops it — the uplink lives.
    Tools { tools: Vec<ToolInfo> },
    /// Result of a [`HubMsg::Command`], correlated by `req`.
    Reply { req: ReqId, result: CmdResult },
    /// Snapshot (RIS-prefixed repaint) for a freshly-attached client; **payload
    /// tail = the snapshot bytes**. Always the FIRST frame on a terminal channel.
    Snapshot { ch: ChannelId },
    /// Live PTY output for a client; **payload tail = raw bytes**.
    Output { ch: ChannelId },
    /// A filesystem-watch event for a watch channel; **payload tail = JSON**.
    WatchEvt { ch: ChannelId },
    /// The channel's session ended (child exited) — the hub closes the client WS.
    Closed { ch: ChannelId },
    /// The agent observed a busy→waiting edge; the hub turns it into a push
    /// notification (centralized push). No payload. `detail` carries the agent's
    /// last cached LLM summary for this session (proposal 0022) so the push body
    /// can be the summary rather than the bare preview; `None` falls back to
    /// `preview`. Additive — an older hub ignores the field.
    WaitingEdge {
        session: String,
        short: String,
        preview: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// The agent asks the hub to summarize one session (proposal 0022). The agent
    /// has redacted `inputs` + `tail` before sending. `content_hash` identifies the
    /// exact (inputs, tail) snapshot so the hub echoes it back and the agent can
    /// drop a stale result. The hub gates on feature/key/budget before spending.
    SummaryRequest {
        machine: String,
        session: String,
        content_hash: u64,
        inputs: Vec<String>,
        tail: String,
    },
    /// Reply to [`HubMsg::Ping`].
    Pong,
}

// ── hub → agent ─────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HubMsg {
    /// A client attached: allocate `ch` to `session` and start streaming
    /// `Snapshot` then `Output`. Mirrors a browser opening `/api/ws`.
    Attach { ch: ChannelId, session: String, cols: u16, rows: u16 },
    /// The client disconnected: end `ch` (→ the agent's `unregister_client`).
    Detach { ch: ChannelId },
    /// The client resized: re-pin the PTY to the per-axis min across clients.
    Resize { ch: ChannelId, cols: u16, rows: u16 },
    /// Client input for `ch`; **payload tail = raw bytes**.
    Input { ch: ChannelId },
    /// Open/refine a filesystem-watch channel (or `unsub` to drop dirs).
    WatchSub { ch: ChannelId, dirs: Vec<String>, unsub: bool },
    /// A small request/reply control op (lifecycle, small file ops).
    Command { req: ReqId, cmd: Cmd },
    /// Ask the agent to dial the dedicated bulk WS for a large transfer. `id` is
    /// an unguessable random nonce the agent presents (with its `machine_id`) on
    /// the dial-back, so only the selected agent can claim this transfer's slot.
    OpenBulk { id: String, bulk: BulkSpec },
    /// The hub's answer to a [`AgentMsg::SummaryRequest`] (proposal 0022). Echoes
    /// `content_hash` so the agent ignores it if the session changed again
    /// meanwhile. `headline`/`detail` are `None` when the hub declined (feature
    /// off, no key, or over budget) — the agent then keeps showing `preview`.
    SummaryResult {
        session: String,
        content_hash: u64,
        headline: Option<String>,
        detail: Option<String>,
    },
    /// Liveness probe (→ [`AgentMsg::Pong`]).
    Ping,
}

/// Small request/reply operations the hub routes to the owning agent. Each maps
/// to existing agent handler logic; the agent answers with [`CmdResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Cmd {
    Create(CreateReq),
    Delete(DeleteReq),
    Key { session: String, key: String },
    Paste { session: String, text: String, enter: bool },
    ClearHistory { session: String },
    /// Set (or clear) a session's operator-chosen mark colour (proposal 0029).
    /// `color` is a curated palette token; `None` clears the mark. The agent
    /// validates the token, persists it on the session + manifest, and replies
    /// with the updated [`SessionInfo`] as [`CmdResult::Json`].
    SetColor { session: String, color: Option<String> },
    /// Set (or clear) a session's operator-chosen display label (proposal 0035).
    /// `label` is free text (trimmed + length-capped); `None`/empty clears it. The
    /// agent normalizes it, persists it on the session + manifest, and replies
    /// with the updated [`SessionInfo`] as [`CmdResult::Json`].
    SetLabel { session: String, label: Option<String> },
    Restore,
    Restorable,
    SessionRoot { session: Option<String> },
    GetFavorites,
    PutFavorites(Vec<Favorite>),
    /// A small $HOME-confined file-browser/editor op (dirs/files/read/write/
    /// delete/mkdir/rmdir/rename). `op` names it; `args` carries its parameters as
    /// JSON. The agent runs the op (confinement enforced agent-side) and replies
    /// with `CmdResult::Json` (a body) or `CmdResult::Ok` (204). Bulk transfers
    /// (download/upload/clipboard image) use the dedicated bulk stream instead.
    File { op: String, args: serde_json::Value },
    /// Start the update-then-restart job on this machine (proposal 0049).
    /// `tools` names the assistant prefixes to update (empty = all); `restart` is
    /// `"updated"` | `"all"` | `"none"`. **No command travels on the wire** — the
    /// agent resolves each name against its own registry. Replies
    /// [`CmdResult::Json`] with the [`crate::UpdateJob`] snapshot (or a `409` error
    /// carrying the running job's).
    /// `install_missing` (proposal 0050) additionally installs the assistants
    /// that aren't on the machine at all, for the local user, before updating the
    /// ones that are. Gated on [`CAP_ASSISTANT_INSTALL`] — see that constant for
    /// why the capability, not the field, is what keeps this honest.
    UpdateAssistants {
        tools: Vec<String>,
        restart: String,
        #[serde(default)]
        install_missing: bool,
    },
    /// Read the current-or-last update job (proposal 0049) → [`CmdResult::Json`].
    UpdateStatus,
    /// Restart **one** named session and resume its conversation (proposal 0087)
    /// — stop the assistant with `/exit` (escalating to a kill), relaunch it
    /// under the same name with `resume = true`, and re-apply its colour and
    /// label. Nothing machine-global changes, which is why this is gated like the
    /// other session ops (`may_see_session`) rather than owner-only: it is
    /// strictly safer than the `/exit` and Delete a grantee can already perform.
    ///
    /// Synchronous: worst case ~15 s (10 s graceful + 5 s forced). Replies
    /// [`CmdResult::Json`] with the [`crate::SessionRestartStatus`] row, or
    /// [`CmdResult::Error`] for a refusal (404 unknown, 422 `shell`, 409 already
    /// restarting / an update job is running). Gated on [`CAP_SESSION_RESTART`].
    RestartSession { session: String },
    /// What installing the missing assistants on this machine WOULD do (proposal
    /// 0050): the exact commands, the missing prerequisites and the size hints,
    /// so the dialog can show them before the user confirms. A pure probe.
    /// Replies [`CmdResult::Json`] with [`crate::InstallPlan`].
    InstallPlan { tools: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CmdResult {
    Ok,
    Created(String),
    Error { code: u16, msg: String },
    Restorable(Vec<RestorableSession>),
    SessionRoot { root: String, home: String },
    Favorites(Vec<Favorite>),
    /// A free-form JSON payload (e.g. the restore result, file-op replies). Used
    /// when a reply doesn't fit a fixed shape; the hub forwards it to the client.
    Json(serde_json::Value),
}

/// A bulk HTTP transfer relayed over the dedicated `/agent/bulk` WS (download,
/// upload, clipboard image). The hub fills this from the client's request; the
/// agent replays it against its **real** file-transfer handlers (so Range,
/// multipart, and `$HOME` confinement behave exactly as a direct connection).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BulkSpec {
    pub method: String,
    /// Path + query, e.g. `/api/download?path=…&inline=1`.
    pub uri: String,
    /// Relayed request headers (Range, Content-Type/boundary, …); hop-by-hop
    /// headers are dropped by the hub.
    pub headers: Vec<(String, String)>,
}

/// The response head the agent sends back (as the first text frame) on the bulk
/// WS, before streaming the body as binary frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BulkRespHead {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// Sentinel text frame marking end-of-request-body (hub→agent) on the bulk WS.
/// The agent ends its response with a WebSocket Close instead.
pub const BULK_BODY_END: &str = "\u{4}";

// ── framing ─────────────────────────────────────────────────────────────────────
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the declared header length (or no room for the length).
    Truncated,
    /// The header bytes weren't valid JSON for the expected message type.
    BadHeader,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Truncated => write!(f, "truncated hub frame"),
            FrameError::BadHeader => write!(f, "malformed hub frame header"),
        }
    }
}

impl std::error::Error for FrameError {}

const LEN_PREFIX: usize = 4;

/// Encode one frame: `[u32 BE header_len][JSON header][payload]`. `payload` is the
/// raw byte tail (empty for control frames). Pass it as one binary WS message.
pub fn encode_frame<T: Serialize>(msg: &T, payload: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(msg).expect("hub frame header serializes");
    let mut out = Vec::with_capacity(LEN_PREFIX + header.len() + payload.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out
}

/// Decode one frame, returning the message and a borrow of its raw payload tail.
/// Never panics or over-reads on malformed input — a bad frame is an `Err` the
/// caller can skip while keeping the connection open.
pub fn decode_frame<T: DeserializeOwned>(buf: &[u8]) -> Result<(T, &[u8]), FrameError> {
    if buf.len() < LEN_PREFIX {
        return Err(FrameError::Truncated);
    }
    let header_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let rest = &buf[LEN_PREFIX..];
    if rest.len() < header_len {
        return Err(FrameError::Truncated);
    }
    let (header, payload) = rest.split_at(header_len);
    let msg = serde_json::from_slice(header).map_err(|_| FrameError::BadHeader)?;
    Ok((msg, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Round-trip helper: encode then decode back into the same type.
    fn round<T: Serialize + DeserializeOwned>(msg: &T, payload: &[u8]) -> (T, Vec<u8>) {
        let buf = encode_frame(msg, payload);
        let (decoded, tail) = decode_frame::<T>(&buf).expect("decodes");
        (decoded, tail.to_vec())
    }

    #[test]
    fn roundtrips_every_agent_frame() {
        let tools = vec![ToolInfo {
            cmd: "cc".into(),
            prefix: "claude".into(),
            extra_dirs: None,
            unavailable: false,
            remote_control_available: true,
        }];
        let sess = SessionInfo {
            name: "claude-x".into(),
            tool: "claude".into(),
            short: "x".into(),
            attached: true,
            activity: 7,
            last_input_at: 8,
            busy_since: 9,
            busy_until: 10,
            preview: "hi".into(),
            waiting: false,
            skip_permissions: Some(true),
            cwd: "/home/u".into(),
            machine: "box1".into(),
            headline: None,
            detail: None,
            color: Some("rose".into()),
            label: None,
            assistant_remote_control: Some(false),
        };
        let cases = vec![
            AgentMsg::Register {
                proto: HUB_PROTO_VERSION,
                machine_id: "box1".into(),
                hostname: "box1.local".into(),
                agent_version: "0.2.2".into(),
                tools: tools.clone(),
                caps: vec![CAP_ASSISTANT_UPDATE.into()],
            },
            AgentMsg::Sessions { sessions: vec![sess.clone()] },
            AgentMsg::Reply { req: 3, result: CmdResult::Created("claude-x".into()) },
            AgentMsg::Snapshot { ch: 1 },
            AgentMsg::Output { ch: 2 },
            AgentMsg::WatchEvt { ch: 5 },
            AgentMsg::Closed { ch: 2 },
            AgentMsg::WaitingEdge { session: "claude-x".into(), short: "x".into(), preview: "done".into(), detail: Some("Paused for tests.".into()) },
            AgentMsg::SummaryRequest {
                machine: "box1".into(),
                session: "claude-x".into(),
                content_hash: 0xdead_beef,
                inputs: vec!["fix the auth bug".into(), "y".into()],
                tail: "login() rewritten\nrun tests? (y/n)".into(),
            },
            AgentMsg::Pong,
        ];
        for m in cases {
            let (back, tail) = round(&m, b"");
            assert_eq!(back, m, "agent frame round-trips");
            assert!(tail.is_empty());
        }
    }

    #[test]
    fn roundtrips_every_hub_frame() {
        let cases = vec![
            HubMsg::Attach { ch: 1, session: "claude-x".into(), cols: 80, rows: 24 },
            HubMsg::Detach { ch: 1 },
            HubMsg::Resize { ch: 1, cols: 100, rows: 40 },
            HubMsg::Input { ch: 1 },
            HubMsg::WatchSub { ch: 9, dirs: vec!["/home/u".into()], unsub: false },
            HubMsg::Command { req: 1, cmd: Cmd::Create(CreateReq::default()) },
            HubMsg::Command { req: 2, cmd: Cmd::Delete(DeleteReq { session: "claude-x".into(), mode: "kill".into() }) },
            HubMsg::Command { req: 3, cmd: Cmd::Key { session: "claude-x".into(), key: "enter".into() } },
            HubMsg::Command { req: 4, cmd: Cmd::SessionRoot { session: None } },
            HubMsg::Command { req: 5, cmd: Cmd::SetColor { session: "claude-x".into(), color: Some("teal".into()) } },
            HubMsg::Command { req: 6, cmd: Cmd::SetColor { session: "claude-x".into(), color: None } },
            HubMsg::Command { req: 7, cmd: Cmd::UpdateAssistants { tools: vec!["claude".into()], restart: "updated".into(), install_missing: false } },
            HubMsg::Command { req: 8, cmd: Cmd::UpdateStatus },
            // Proposal 0050: installing what's missing, and asking what that would run.
            HubMsg::Command { req: 9, cmd: Cmd::UpdateAssistants { tools: vec![], restart: "updated".into(), install_missing: true } },
            HubMsg::Command { req: 10, cmd: Cmd::InstallPlan { tools: vec!["kimi".into()] } },
            // Proposal 0087: restart one named session and resume it.
            HubMsg::Command { req: 11, cmd: Cmd::RestartSession { session: "claude-x".into() } },
            HubMsg::OpenBulk { id: "nonce-abc123".into(), bulk: BulkSpec { method: "GET".into(), uri: "/api/download?path=/home/u/f".into(), headers: vec![("range".into(), "bytes=0-99".into())] } },
            HubMsg::SummaryResult {
                session: "claude-x".into(),
                content_hash: 0xdead_beef,
                headline: Some("Waiting to run tests".into()),
                detail: Some("It refactored auth and is paused.".into()),
            },
            HubMsg::Ping,
        ];
        for m in cases {
            let (back, tail) = round(&m, b"");
            assert_eq!(back, m, "hub frame round-trips");
            assert!(tail.is_empty());
        }
    }

    // Proposal 0087 inserted `Cmd::RestartSession` into the middle of the enum.
    // Serde's externally-tagged representation keys on the variant NAME, so that
    // cannot move an existing variant — but hub and agent are deployed
    // independently, so pin the encodings that a rename would silently break.
    #[test]
    fn existing_cmd_variants_serialize_unchanged() {
        assert_eq!(serde_json::to_string(&Cmd::UpdateStatus).unwrap(), "\"UpdateStatus\"");
        assert_eq!(
            serde_json::to_string(&Cmd::Key { session: "claude-x".into(), key: "enter".into() })
                .unwrap(),
            r#"{"Key":{"session":"claude-x","key":"enter"}}"#
        );
        assert_eq!(
            serde_json::to_string(&Cmd::RestartSession { session: "claude-x".into() }).unwrap(),
            r#"{"RestartSession":{"session":"claude-x"}}"#
        );
    }

    #[test]
    fn output_payload_is_byte_exact_and_not_lossy() {
        // Non-UTF-8 PTY bytes (ESC, NUL, 0xff) and the empty payload must survive
        // verbatim — the whole reason the tail is raw, not JSON/base64.
        for payload in [&b"\xff\xfe\x00\x1bc"[..], &b""[..], &[0u8; 4096][..]] {
            let (msg, tail) = round(&AgentMsg::Output { ch: 7 }, payload);
            assert_eq!(msg, AgentMsg::Output { ch: 7 });
            assert_eq!(tail, payload, "payload bytes are byte-identical");
        }
    }

    #[test]
    fn handles_large_payload() {
        // 1 MiB tail — guards the u32 length prefix (a u16 would have truncated).
        let payload = vec![0xab_u8; 1 << 20];
        let (_, tail) = round(&AgentMsg::Output { ch: 1 }, &payload);
        assert_eq!(tail.len(), 1 << 20);
        assert!(tail.iter().all(|&b| b == 0xab));
    }

    #[test]
    fn snapshot_preserves_ris_prefix() {
        let body = [crate::SNAPSHOT_RESET, b"hello"].concat();
        let (msg, tail) = round(&AgentMsg::Snapshot { ch: 1 }, &body);
        assert_eq!(msg, AgentMsg::Snapshot { ch: 1 });
        assert!(tail.starts_with(crate::SNAPSHOT_RESET), "RIS prefix survives: {tail:?}");
    }

    #[test]
    fn decode_rejects_truncated_frame() {
        // Header claims 100 bytes; far fewer present.
        let mut buf = (100u32).to_be_bytes().to_vec();
        buf.extend_from_slice(b"{partial");
        assert_eq!(decode_frame::<AgentMsg>(&buf), Err(FrameError::Truncated));
        // Even the length prefix is incomplete.
        assert_eq!(decode_frame::<AgentMsg>(&[0, 0]), Err(FrameError::Truncated));
    }

    #[test]
    fn decode_rejects_bad_header() {
        // Well-framed length, but the header isn't valid JSON for AgentMsg.
        let header = b"not json";
        let mut buf = (header.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(header);
        assert_eq!(decode_frame::<AgentMsg>(&buf), Err(FrameError::BadHeader));
    }

    // `caps` is additive (proposal 0049): a pre-0049 agent's Register carries no
    // such field and must still decode — the hub reads the absence as "can't
    // self-update" and answers 501, rather than failing the whole registration.
    #[test]
    fn register_without_caps_still_decodes() {
        let old = serde_json::json!({
            "Register": {
                "proto": HUB_PROTO_VERSION,
                "machine_id": "oldbox",
                "hostname": "oldbox.local",
                "agent_version": "0.3.40",
                "tools": [],
            }
        });
        let header = serde_json::to_vec(&old).unwrap();
        let mut buf = (header.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&header);
        let (msg, _) = decode_frame::<AgentMsg>(&buf).expect("old Register decodes");
        match msg {
            AgentMsg::Register { caps, machine_id, .. } => {
                assert_eq!(machine_id, "oldbox");
                assert!(caps.is_empty(), "absent caps ⇒ empty, not an error");
            }
            other => panic!("expected Register, got {other:?}"),
        }
        // …and a capability-less Register serializes without the field at all,
        // so an older hub sees byte-identical JSON.
        let fresh = AgentMsg::Register {
            proto: HUB_PROTO_VERSION,
            machine_id: "box".into(),
            hostname: "box".into(),
            agent_version: "0".into(),
            tools: vec![],
            caps: vec![],
        };
        let s = serde_json::to_string(&fresh).unwrap();
        assert!(!s.contains("caps"), "empty caps is omitted: {s}");
    }

    #[test]
    fn unauthorized_close_code_is_private_use() {
        // Must sit in the WebSocket application/private-use range (4000–4999) so it
        // never collides with a protocol-defined close code (proposal 0048).
        assert!((4000..=4999).contains(&UPLINK_CLOSE_UNAUTHORIZED));
    }
}
