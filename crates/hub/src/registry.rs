//! The registry of connected agents and the live relay routing.
//!
//! Each agent is an [`AgentConn`] shared as `Arc`: the uplink server (which owns
//! the agent WS) and the client bridges (which own browser WSes) both hold it.
//! `to_agent` funnels encoded `HubMsg` frames to the agent; `channels` maps each
//! attached client's [`ChannelId`] to a sink toward its browser WS. An agent that
//! drops its uplink is *greyed* (kept, marked offline, its last session list
//! retained) rather than vanishing.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cc_screen_protocol::hub::{encode_frame, ChannelId, Cmd, CmdResult, HubMsg, ReqId};
pub use cc_screen_protocol::MachineInfo;
use cc_screen_protocol::{SessionInfo, ToolInfo};
use tokio::sync::{mpsc, oneshot};

/// How long the hub waits for an agent to answer a control op before giving up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a routed control op didn't get a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestErr {
    /// The agent's uplink is gone (send failed, or it went offline mid-flight).
    Offline,
    /// The agent didn't reply within the timeout.
    Timeout,
}

/// What a client request is allowed to see/reach — ownership (proposal 0001 §4.1)
/// **plus** sharing grants (proposal 0039). Derived once per request from the
/// session cookie + the caller's `shares` rows by the auth middleware and passed
/// to every registry lookup, so tenant isolation *and* sharing are enforced in
/// one place.
///
/// Single-tenant is always [`Visibility::All`] — an unconditional yes that never
/// consults a grant. Multi-tenant is [`Visibility::User`], carrying the resolved
/// union of that user's grants ([`UserVis`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Single-tenant (or an admin): every agent, every session.
    All,
    /// Multi-tenant: this user owns some agents and may have grants on others.
    User(UserVis),
}

/// One non-owner's access to an agent they do **not** own — the resolved union of
/// that user's `shares` rows for it (proposal 0039 §2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Grant {
    /// `kind='agent'`: use the whole agent (list/create/attach). The owner-peek
    /// flag is the OWNER's concern, not the grantee's, so it isn't carried here.
    Agent,
    /// `kind='session'`: view only these session names; agent-scope is denied.
    Sessions(HashSet<String>),
}

/// A multi-tenant caller's resolved visibility: the agents they own (by
/// `user_id`) plus the grants they hold on others, and — for agents they *own* —
/// the owner-peek and session-share-back overrides that widen their own list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserVis {
    /// The caller.
    pub user_id: String,
    /// `agent_id` → the grant this user holds on an agent they do NOT own.
    grants: HashMap<String, Grant>,
    /// For agents this user OWNS: the grantee `user_id`s whose created sessions the
    /// owner opted to keep sight of (owner-peek, §4.3), keyed by `agent_id`.
    peek: HashMap<String, HashSet<String>>,
    /// For agents this user OWNS: specific session names surfaced back to the owner
    /// by a session-share-back (§3.1 rule 3), keyed by `agent_id`.
    shared_back: HashMap<String, HashSet<String>>,
}

/// One `shares` row joined to its agent's owner — the plain data the multi-tenant
/// [`crate::db::Store`] returns and [`Visibility::from_rows`] folds into a
/// [`UserVis`]. Defined here (not in the feature-gated `db`) so the predicate that
/// consumes it stays in one place and compiles in every build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareRow {
    pub id: String,
    pub agent_id: String,
    /// `agents.user_id` — the owner, resolved by the join.
    pub owner_user_id: String,
    pub grantee_user_id: String,
    /// `"agent"` or `"session"`.
    pub kind: String,
    pub session: Option<String>,
    pub owner_peek: bool,
    pub created_at: i64,
}

impl Visibility {
    /// An empty multi-tenant visibility for `user_id` (owns whatever agents carry
    /// that `user_id`, holds no grants). Used for tests and for the exempt
    /// no-session request (empty id matches no agent).
    pub fn user(user_id: impl Into<String>) -> Self {
        Visibility::User(UserVis { user_id: user_id.into(), ..Default::default() })
    }

    /// Build a caller's visibility from their loaded `shares` rows (those granting
    /// INTO them, and those they issued OUT of agents they own). The row set is the
    /// union `grantee_user_id = user_id OR owner_user_id = user_id`.
    pub fn from_rows(user_id: String, rows: Vec<ShareRow>) -> Self {
        let mut v = UserVis { user_id: user_id.clone(), ..Default::default() };
        for r in rows {
            // Grants INTO this user (on agents someone else owns).
            if r.grantee_user_id == user_id && r.owner_user_id != user_id {
                match r.kind.as_str() {
                    "agent" => {
                        v.grants.insert(r.agent_id.clone(), Grant::Agent);
                    }
                    "session" => {
                        if let Some(s) = r.session.clone() {
                            match v.grants.entry(r.agent_id.clone()).or_insert_with(|| Grant::Sessions(HashSet::new())) {
                                // An agent grant supersedes a session grant — leave it.
                                Grant::Agent => {}
                                Grant::Sessions(set) => {
                                    set.insert(s);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // A session shared BACK to this user on an agent they own (§3.1 rule 3).
            if r.grantee_user_id == user_id && r.owner_user_id == user_id && r.kind == "session" {
                if let Some(s) = r.session.clone() {
                    v.shared_back.entry(r.agent_id.clone()).or_default().insert(s);
                }
            }
            // Owner-peek this user issued on an agent they own (§4.3).
            if r.owner_user_id == user_id && r.kind == "agent" && r.owner_peek {
                v.peek.entry(r.agent_id.clone()).or_default().insert(r.grantee_user_id.clone());
            }
        }
        Visibility::User(v)
    }

    /// May the caller USE this agent as a whole — list its sessions, create new
    /// ones, attach to any? True iff they own it or hold an agent-grant.
    pub fn may_use_agent(&self, agent: &AgentConn) -> bool {
        match self {
            Visibility::All => true,
            Visibility::User(v) => {
                agent.user_id == v.user_id
                    || matches!(v.grants.get(&agent.agent_id), Some(Grant::Agent))
            }
        }
    }

    /// May the caller VIEW/attach this specific session on this agent? True if they
    /// may use the agent, OR a session-grant (or a session-share-back) names exactly
    /// this session.
    pub fn may_see_session(&self, agent: &AgentConn, session: &str) -> bool {
        if self.may_use_agent(agent) {
            return true;
        }
        match self {
            Visibility::All => true,
            Visibility::User(v) => {
                matches!(v.grants.get(&agent.agent_id), Some(Grant::Sessions(set)) if set.contains(session))
                    || v.shared_back.get(&agent.agent_id).is_some_and(|s| s.contains(session))
            }
        }
    }

    /// Does the caller have *any* visibility on this agent — enough to list the
    /// machine? True if they may use it, or hold at least one session grant on it.
    pub fn has_any_visibility(&self, agent: &AgentConn) -> bool {
        match self {
            Visibility::All => true,
            Visibility::User(v) => {
                agent.user_id == v.user_id || v.grants.contains_key(&agent.agent_id)
            }
        }
    }

    /// Should `session` (created by `creator`) appear in the caller's session list
    /// for `agent`? This is the attribution-aware rule the owner/grantee asymmetry
    /// (§3.1) and owner-peek (§4.3) live in — distinct from `may_see_session`
    /// (which governs *attach* and treats any owner/agent-grantee as all-seeing).
    pub fn lists_session(&self, agent: &AgentConn, session: &str, creator: &str) -> bool {
        match self {
            Visibility::All => true,
            Visibility::User(v) => {
                let aid = &agent.agent_id;
                if agent.user_id == v.user_id {
                    // Owner: their own sessions always; a grantee's only under peek
                    // for that grantee, or if the grantee shared the session back.
                    creator == v.user_id
                        || v.peek.get(aid).is_some_and(|s| s.contains(creator))
                        || v.shared_back.get(aid).is_some_and(|s| s.contains(session))
                } else if matches!(v.grants.get(aid), Some(Grant::Agent)) {
                    // Agent-grantee: the owner's sessions + their own (not a third
                    // grantee's).
                    creator == agent.user_id || creator == v.user_id
                } else if let Some(Grant::Sessions(set)) = v.grants.get(aid) {
                    // Session-grantee: only the named session(s).
                    set.contains(session)
                } else {
                    false
                }
            }
        }
    }
}

/// A frame headed to one attached browser/TUI client (keyed by its `ch`).
#[derive(Debug)]
pub enum ToBrowser {
    /// Raw terminal bytes (a snapshot or live output) → a binary WS frame.
    Bytes(Vec<u8>),
    /// The session ended (or the agent went offline) → close the browser WS.
    Close,
}

/// One connected (or recently-connected, now-greyed) agent.
pub struct AgentConn {
    /// The hub's durable, globally-unique identity for this agent and the registry
    /// key. Single-tenant: equal to `machine_id`. Multi-tenant: the `agents.id`
    /// row, so two tenants' identically-labelled machines never collide.
    pub agent_id: String,
    /// The owning tenant (proposal 0001). `cc_screen_auth::OWNER` in single-tenant.
    pub user_id: String,
    /// The user-facing machine label ("laptop"); what clients pass as `?machine=`
    /// and what session rows are tagged with. Unique only *within* a tenant.
    pub machine_id: String,
    pub hostname: String,
    pub tools: Vec<ToolInfo>,
    online: AtomicBool,
    last_sessions: Mutex<Vec<SessionInfo>>,
    /// Encoded `HubMsg` frames → the agent's WS writer (drained by the uplink
    /// server's writer task).
    to_agent: mpsc::Sender<Vec<u8>>,
    /// Allocates per-client channel ids (0 is the control channel, so we start 1).
    next_ch: AtomicU32,
    /// ch → sink toward that client's browser WS.
    channels: Mutex<HashMap<ChannelId, mpsc::Sender<ToBrowser>>>,
    /// Allocates request ids for control ops.
    next_req: AtomicU32,
    /// In-flight control ops awaiting an `AgentMsg::Reply`, keyed by request id.
    pending: Mutex<HashMap<ReqId, oneshot::Sender<CmdResult>>>,
    /// Creator attribution (proposal 0039 §4.1): `session_name → creator user_id`
    /// for sessions the hub relayed a non-owner's `Create` for. Ephemeral and
    /// hub-side — a visibility hint, not a security boundary (the boundary is the
    /// agent grant that let the non-owner create at all). A session not present here
    /// is the owner's; lost on hub restart (sessions then fall back to owner).
    creators: Mutex<HashMap<String, String>>,
}

impl AgentConn {
    pub fn online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// The sender for encoded `HubMsg` frames to this agent.
    pub fn to_agent(&self) -> &mpsc::Sender<Vec<u8>> {
        &self.to_agent
    }

    pub fn set_sessions(&self, sessions: Vec<SessionInfo>) {
        *self.last_sessions.lock().unwrap() = sessions;
    }

    /// This agent's sessions, each stamped with its `machine`.
    pub fn sessions_tagged(&self) -> Vec<SessionInfo> {
        self.last_sessions
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|mut s| {
                s.machine = self.machine_id.clone();
                s
            })
            .collect()
    }

    /// Allocate a fresh channel id and register a browser sink for it.
    pub fn open_channel(&self, sink: mpsc::Sender<ToBrowser>) -> ChannelId {
        let ch = self.next_ch.fetch_add(1, Ordering::Relaxed) + 1; // 0 = control
        self.channels.lock().unwrap().insert(ch, sink);
        ch
    }

    /// Drop a client channel (browser disconnected / detached).
    pub fn close_channel(&self, ch: ChannelId) {
        self.channels.lock().unwrap().remove(&ch);
    }

    /// Route an agent→client frame to the browser sink for `ch`. Awaits the sink
    /// (backpressure), so a slow browser slows this agent's read loop, which
    /// backpressures the agent and ultimately triggers its `Lagged`→resync rather
    /// than dropping bytes. Does not hold the channels lock across the await.
    pub async fn route_to_browser(&self, ch: ChannelId, msg: ToBrowser) {
        let sink = self.channels.lock().unwrap().get(&ch).cloned();
        if let Some(s) = sink {
            let _ = s.send(msg).await;
        }
    }

    /// Send a control op to the agent and await its reply (correlated by a fresh
    /// request id). Returns `Offline` if the uplink is gone, `Timeout` if the
    /// agent doesn't answer in time.
    pub async fn request(&self, cmd: Cmd) -> Result<CmdResult, RequestErr> {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(req, tx);
        let frame = encode_frame(&HubMsg::Command { req, cmd }, b"");
        if self.to_agent.send(frame).await.is_err() {
            self.pending.lock().unwrap().remove(&req);
            return Err(RequestErr::Offline);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => Ok(result),
            // Timed out, or the sender was dropped (agent went offline → pending
            // drained). Clean up the slot either way.
            _ => {
                self.pending.lock().unwrap().remove(&req);
                Err(RequestErr::Timeout)
            }
        }
    }

    /// Resolve an in-flight control op with the agent's reply.
    pub fn resolve_reply(&self, req: ReqId, result: CmdResult) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&req) {
            let _ = tx.send(result);
        }
    }

    /// On agent disconnect: mark offline, fail every in-flight request, and close
    /// every bridged browser (their sessions are unreachable until the agent
    /// reconnects). The entry + its last session list are retained so the UI greys
    /// the machine.
    pub fn go_offline(&self) {
        self.online.store(false, Ordering::Relaxed);
        // Drop pending reply senders → awaiting `request()` calls error out.
        self.pending.lock().unwrap().clear();
        let chans: Vec<_> = self.channels.lock().unwrap().drain().map(|(_, s)| s).collect();
        for s in chans {
            let _ = s.try_send(ToBrowser::Close);
        }
    }
}

#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<HashMap<String, Arc<AgentConn>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Single-tenant register: the agent is owned by `cc_screen_auth::OWNER` and
    /// its `agent_id` is its `machine_id` (so the registry key is unchanged from
    /// before multi-tenancy). The byte-for-byte path the local uplink uses.
    pub fn register(
        &self,
        machine_id: &str,
        hostname: &str,
        tools: Vec<ToolInfo>,
        to_agent: mpsc::Sender<Vec<u8>>,
    ) -> Arc<AgentConn> {
        self.register_agent(machine_id, cc_screen_auth::OWNER, machine_id, hostname, tools, to_agent)
    }

    /// Register a freshly-connected agent (online), keyed by its globally-unique
    /// `agent_id`, returning the shared conn. Replaces any prior entry for the same
    /// `agent_id` (a reconnect) but carries over the last-known session list so the
    /// UI doesn't blink empty before the first poll.
    pub fn register_agent(
        &self,
        agent_id: &str,
        user_id: &str,
        machine_id: &str,
        hostname: &str,
        tools: Vec<ToolInfo>,
        to_agent: mpsc::Sender<Vec<u8>>,
    ) -> Arc<AgentConn> {
        let mut g = self.inner.lock().unwrap();
        let carried = g.get(agent_id).map(|c| c.sessions_tagged_raw()).unwrap_or_default();
        let conn = Arc::new(AgentConn {
            agent_id: agent_id.to_string(),
            user_id: user_id.to_string(),
            machine_id: machine_id.to_string(),
            hostname: hostname.to_string(),
            tools,
            online: AtomicBool::new(true),
            last_sessions: Mutex::new(carried),
            to_agent,
            next_ch: AtomicU32::new(0),
            channels: Mutex::new(HashMap::new()),
            next_req: AtomicU32::new(0),
            pending: Mutex::new(HashMap::new()),
            creators: Mutex::new(HashMap::new()),
        });
        g.insert(agent_id.to_string(), conn.clone());
        conn
    }

    /// Look up an agent by its registry key (`agent_id`; in single-tenant this is
    /// the `machine_id`).
    pub fn get(&self, agent_id: &str) -> Option<Arc<AgentConn>> {
        self.inner.lock().unwrap().get(agent_id).cloned()
    }

    /// Resolve which agent a client request targets.
    ///
    /// With an explicit `machine`, that agent (if online). Without one — e.g. the
    /// React PWA, which doesn't thread `machine` through yet — fall back: the
    /// single online agent that owns `session`, or (when `session` is `None`, e.g.
    /// restore/file ops) the only online agent. `None` if unknown / offline /
    /// ambiguous (more than one machine could match). A client that DOES send
    /// `machine` is always unambiguous; this only kicks in for machine-less ones.
    pub fn resolve(&self, machine: &str, session: Option<&str>) -> Option<Arc<AgentConn>> {
        self.resolve_scoped(&Visibility::All, machine, session)
    }

    /// Visibility-scoped resolve — the §4.1 keystone, widened by sharing (0039).
    /// Only agents `vis` allows are considered; `machine` matches the user-facing
    /// `machine_id` label (NOT the registry key), so a multi-tenant client naming
    /// "laptop" reaches *its own* laptop and can never reach another tenant's
    /// identically-named machine. When a `session` is given the filter is
    /// `may_see_session` (so a session-grantee resolves to the owning agent for
    /// that session only); without one it is `may_use_agent` (so a session-grantee
    /// has no usable agent for a machine-less create/restore). Single-tenant
    /// (`Visibility::All`) reproduces the prior behavior exactly.
    pub fn resolve_scoped(
        &self,
        vis: &Visibility,
        machine: &str,
        session: Option<&str>,
    ) -> Option<Arc<AgentConn>> {
        let g = self.inner.lock().unwrap();
        // The per-request visibility filter: a session pins to `may_see_session`,
        // otherwise the agent must be usable as a whole.
        let allowed = |a: &&Arc<AgentConn>| {
            a.online()
                && match session {
                    Some(s) => vis.may_see_session(a, s),
                    None => vis.may_use_agent(a),
                }
        };
        if !machine.is_empty() {
            let mut it = g.values().filter(allowed).filter(|a| a.machine_id == machine);
            let first = it.next()?;
            // Unique per tenant; a second match (only possible cross-tenant, which
            // scope already excludes) is treated as ambiguous → refuse.
            if it.next().is_some() {
                return None;
            }
            return Some(first.clone());
        }
        let online: Vec<&Arc<AgentConn>> = g.values().filter(allowed).collect();
        match session {
            Some(name) => {
                let mut owners = online.into_iter().filter(|a| a.owns(name));
                let first = owners.next()?;
                // Ambiguous if a second visible machine also has a session by that name.
                if owners.next().is_some() {
                    return None;
                }
                Some(first.clone())
            }
            // No session to disambiguate by: only safe when there's exactly one.
            None => match online.as_slice() {
                [only] => Some((*only).clone()),
                _ => None,
            },
        }
    }

    pub fn is_online(&self, agent_id: &str) -> bool {
        self.get(agent_id).is_some_and(|c| c.online())
    }

    /// Union of every agent's sessions (single-tenant). See [`all_sessions_for`].
    pub fn all_sessions(&self) -> Vec<SessionInfo> {
        self.all_sessions_for(&Visibility::All)
    }

    /// The sessions the caller may list, each machine-tagged, sorted by
    /// `(machine, name)` so identical names on different machines never collide.
    /// Per-session (not just per-agent): a session-only grantee sees exactly their
    /// shared session(s); an owner sees their own plus any a grantee created under
    /// owner-peek or shared back; an agent-grantee sees the owner's plus their own
    /// (proposal 0039 §3.2 / §4.1 attribution).
    pub fn all_sessions_for(&self, vis: &Visibility) -> Vec<SessionInfo> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<SessionInfo> = g
            .values()
            .filter(|c| vis.has_any_visibility(c))
            .flat_map(|c| {
                c.sessions_tagged().into_iter().filter(|s| {
                    let creator = c.creator_of(&s.name).unwrap_or_else(|| c.user_id.clone());
                    vis.lists_session(c, &s.name, &creator)
                })
            })
            .collect();
        out.sort_by(|a, b| (&a.machine, &a.name).cmp(&(&b.machine, &b.name)));
        out
    }

    /// The machine list for the picker / offline greying (single-tenant).
    pub fn machines(&self) -> Vec<MachineInfo> {
        self.machines_for(&Visibility::All)
    }

    /// The machines the caller has any visibility on, sorted by label. A machine is
    /// listed if the caller may use it OR holds at least one session grant on it (so
    /// a session-only grantee still sees the box its shared session lives on).
    pub fn machines_for(&self, vis: &Visibility) -> Vec<MachineInfo> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<MachineInfo> = g
            .values()
            .filter(|c| vis.has_any_visibility(c))
            .map(|c| MachineInfo {
                machine: c.machine_id.clone(),
                hostname: c.hostname.clone(),
                online: c.online(),
            })
            .collect();
        v.sort_by(|a, b| a.machine.cmp(&b.machine));
        v
    }
}

impl AgentConn {
    /// The retained session list *without* re-stamping the machine (used when
    /// carrying it across a reconnect — `sessions_tagged` re-stamps on read).
    fn sessions_tagged_raw(&self) -> Vec<SessionInfo> {
        self.last_sessions.lock().unwrap().clone()
    }

    /// Does this agent currently report a session by this name? (Used by
    /// `Registry::resolve` to route a machine-less request to its owner.)
    fn owns(&self, name: &str) -> bool {
        self.last_sessions.lock().unwrap().iter().any(|s| s.name == name)
    }
}

impl AgentConn {
    /// Attribute a session to its creating (non-owner) user — proposal 0039 §4.1.
    /// Stamped when the hub relays a grantee's `Create` so the owner's list can
    /// honour the share asymmetry (and owner-peek can override it).
    pub fn set_creator(&self, session: &str, user_id: &str) {
        self.creators.lock().unwrap().insert(session.to_string(), user_id.to_string());
    }

    /// The attributed creator of a session, if the hub saw a non-owner create it.
    /// `None` ⇒ attribute to the owner (pre-existing or owner-created).
    pub fn creator_of(&self, session: &str) -> Option<String> {
        self.creators.lock().unwrap().get(session).cloned()
    }

    /// Drop a session's attribution (on delete), so a later identically-named
    /// session isn't mis-attributed to the deleted one's creator.
    pub fn forget_creator(&self, session: &str) {
        self.creators.lock().unwrap().remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_agent() -> mpsc::Sender<Vec<u8>> {
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(8);
        tx
    }

    fn sess(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: "claude".into(),
            short: name.into(),
            attached: false,
            activity: 0,
            last_input_at: 0,
            busy_since: 0,
            busy_until: 0,
            preview: String::new(),
            waiting: false,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
            label: None,
        }
    }

    #[test]
    fn union_session_list_tags_machine() {
        let r = Registry::new();
        let a = r.register("alpha", "alpha.local", vec![], dummy_agent());
        let b = r.register("beta", "beta.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-x"), sess("codex-y")]);
        b.set_sessions(vec![sess("claude-x")]);

        let all = r.all_sessions();
        assert_eq!(all.len(), 3);
        let tagged: Vec<(&str, &str)> =
            all.iter().map(|s| (s.machine.as_str(), s.name.as_str())).collect();
        assert!(tagged.contains(&("alpha", "claude-x")));
        assert!(tagged.contains(&("alpha", "codex-y")));
        assert!(tagged.contains(&("beta", "claude-x")));
        // Same name on two machines stays distinct.
        assert_eq!(all.iter().filter(|s| s.name == "claude-x").count(), 2);
    }

    #[test]
    fn offline_retains_list_and_reregister_restores() {
        let r = Registry::new();
        let a = r.register("alpha", "alpha.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-x")]);
        a.go_offline();

        assert!(!r.is_online("alpha"));
        assert_eq!(r.all_sessions().len(), 1, "list retained while offline");
        assert!(!r.machines()[0].online);

        // Reconnect: online again, and the carried list survives until the next poll.
        let _a2 = r.register("alpha", "alpha.local", vec![], dummy_agent());
        assert!(r.is_online("alpha"));
        assert_eq!(r.all_sessions().len(), 1, "session list carried across reconnect");
    }

    #[tokio::test]
    async fn channels_allocate_route_and_close() {
        let r = Registry::new();
        let a = r.register("alpha", "a", vec![], dummy_agent());

        let (b1, mut b1_rx) = mpsc::channel::<ToBrowser>(8);
        let (b2, _b2_rx) = mpsc::channel::<ToBrowser>(8);
        let ch1 = a.open_channel(b1);
        let ch2 = a.open_channel(b2);
        assert!(ch1 >= 1 && ch2 >= 1 && ch1 != ch2, "ids are unique and skip 0 (control)");

        // Route bytes to ch1's browser sink.
        a.route_to_browser(ch1, ToBrowser::Bytes(b"\x1bchi".to_vec())).await;
        match b1_rx.try_recv() {
            Ok(ToBrowser::Bytes(b)) => assert_eq!(b, b"\x1bchi"),
            other => panic!("expected bytes, got {other:?}"),
        }

        // Closing a channel drops its sink: routing afterward is a no-op.
        a.close_channel(ch1);
        a.route_to_browser(ch1, ToBrowser::Bytes(b"late".to_vec())).await;
        assert!(b1_rx.try_recv().is_err(), "no delivery after close");
    }

    #[tokio::test]
    async fn go_offline_closes_all_browser_channels() {
        let r = Registry::new();
        let a = r.register("alpha", "a", vec![], dummy_agent());
        let (b1, mut b1_rx) = mpsc::channel::<ToBrowser>(8);
        a.open_channel(b1);

        a.go_offline();
        assert!(matches!(b1_rx.recv().await, Some(ToBrowser::Close)), "browser told to close");
        assert!(!a.online());
    }

    #[test]
    fn resolve_by_machine_session_or_single_online() {
        let r = Registry::new();
        let a = r.register("alpha", "a", vec![], dummy_agent());
        let b = r.register("beta", "b", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-x")]);
        b.set_sessions(vec![sess("codex-y"), sess("dup")]);
        a.set_sessions(vec![sess("claude-x"), sess("dup")]); // both have "dup"

        let id = |c: Option<Arc<AgentConn>>| c.map(|x| x.machine_id.clone());

        // Explicit machine wins.
        assert_eq!(id(r.resolve("beta", None)), Some("beta".into()));
        // Machine-less: resolved by the unique owner of the session.
        assert_eq!(id(r.resolve("", Some("claude-x"))), Some("alpha".into()));
        assert_eq!(id(r.resolve("", Some("codex-y"))), Some("beta".into()));
        // Ambiguous (two machines own "dup") or unknown → None.
        assert!(r.resolve("", Some("dup")).is_none());
        assert!(r.resolve("", Some("nope")).is_none());
        // Machine-less with no session + >1 online → ambiguous.
        assert!(r.resolve("", None).is_none());

        // Offline machines don't resolve; once only one is online, the no-session
        // fallback picks it.
        a.go_offline();
        assert!(r.resolve("alpha", None).is_none());
        assert!(r.resolve("", Some("claude-x")).is_none());
        assert_eq!(id(r.resolve("", None)), Some("beta".into()));
    }

    // The §4.1 keystone at the registry layer: two tenants, each with a "laptop",
    // are fully isolated — a scoped resolve/list never crosses the boundary, even
    // though the machine label collides.
    #[test]
    fn tenant_scope_isolates_resolve_and_lists() {
        let r = Registry::new();
        // alice/laptop and bob/laptop — same label, distinct agent ids + owners.
        let a = r.register_agent("agent-a", "alice", "laptop", "a.local", vec![], dummy_agent());
        let b = r.register_agent("agent-b", "bob", "laptop", "b.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-a")]);
        b.set_sessions(vec![sess("claude-b")]);
        let alice = Visibility::user("alice");
        let bob = Visibility::user("bob");
        let id = |c: Option<Arc<AgentConn>>| c.map(|x| x.agent_id.clone());

        // Each tenant's "laptop" resolves to their own agent — never the other's.
        assert_eq!(id(r.resolve_scoped(&alice, "laptop", None)), Some("agent-a".into()));
        assert_eq!(id(r.resolve_scoped(&bob, "laptop", None)), Some("agent-b".into()));
        // A tenant cannot reach the other's session by name.
        assert!(r.resolve_scoped(&alice, "", Some("claude-b")).is_none());
        assert_eq!(id(r.resolve_scoped(&alice, "", Some("claude-a"))), Some("agent-a".into()));
        // Lists are scoped: each tenant sees only their own session + machine.
        assert_eq!(r.all_sessions_for(&alice).len(), 1);
        assert_eq!(r.all_sessions_for(&alice)[0].name, "claude-a");
        assert_eq!(r.machines_for(&bob).len(), 1);
        // The unscoped (single-tenant) views still see everything.
        assert_eq!(r.all_sessions().len(), 2);
        assert_eq!(r.machines().len(), 2);
    }

    // ── Proposal 0039: sharing visibility ─────────────────────────────────────
    fn row(id: &str, agent: &str, owner: &str, grantee: &str, kind: &str, session: Option<&str>, peek: bool) -> ShareRow {
        ShareRow {
            id: id.into(),
            agent_id: agent.into(),
            owner_user_id: owner.into(),
            grantee_user_id: grantee.into(),
            kind: kind.into(),
            session: session.map(Into::into),
            owner_peek: peek,
            created_at: 0,
        }
    }

    // A agent-shares to B: B sees A's machine + sessions, may create + attach;
    // and B's created sessions stay hidden from A unless owner-peek is on.
    #[test]
    fn agent_share_visibility_and_asymmetry() {
        let r = Registry::new();
        let a = r.register_agent("agent-a", "alice", "laptop", "a.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-a"), sess("claude-bee")]);
        a.set_creator("claude-bee", "bob"); // bob created this on alice's agent

        // No shares: bob sees nothing of alice's.
        let bob0 = Visibility::user("bob");
        assert_eq!(r.all_sessions_for(&bob0).len(), 0);
        assert!(r.machines_for(&bob0).is_empty());
        assert!(r.resolve_scoped(&bob0, "laptop", None).is_none());

        // Alice agent-shares to bob (peek off).
        let bob = Visibility::from_rows("bob".into(), vec![row("s1", "agent-a", "alice", "bob", "agent", None, false)]);
        assert_eq!(r.machines_for(&bob).len(), 1, "bob sees alice's machine");
        // Bob, an agent-grantee, sees alice's session AND his own — both.
        let names: HashSet<String> = r.all_sessions_for(&bob).into_iter().map(|s| s.name).collect();
        assert_eq!(names, HashSet::from(["claude-a".into(), "claude-bee".into()]));
        // Bob can create (machine-less resolve passes may_use_agent) and attach.
        assert!(r.resolve_scoped(&bob, "laptop", None).is_some());
        assert!(r.resolve_scoped(&bob, "", Some("claude-a")).is_some());

        // Alice (owner, peek off): her own session yes, bob's hidden.
        let alice = Visibility::user("alice");
        let anames: HashSet<String> = r.all_sessions_for(&alice).into_iter().map(|s| s.name).collect();
        assert_eq!(anames, HashSet::from(["claude-a".into()]), "bob's session hidden from alice");

        // With owner-peek on, alice regains sight of bob's session.
        let alice_peek = Visibility::from_rows("alice".into(), vec![row("s1", "agent-a", "alice", "bob", "agent", None, true)]);
        let pnames: HashSet<String> = r.all_sessions_for(&alice_peek).into_iter().map(|s| s.name).collect();
        assert_eq!(pnames, HashSet::from(["claude-a".into(), "claude-bee".into()]));
    }

    // A session-shares only S to B: B sees exactly S, cannot use the agent, cannot
    // attach to other sessions; a third user with no grant sees nothing.
    #[test]
    fn session_share_is_view_only_and_scoped() {
        let r = Registry::new();
        let a = r.register_agent("agent-a", "alice", "laptop", "a.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("shared"), sess("secret")]);

        let bob = Visibility::from_rows("bob".into(), vec![row("s2", "agent-a", "alice", "bob", "session", Some("shared"), false)]);
        // The list is exactly [shared].
        let names: Vec<String> = r.all_sessions_for(&bob).into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["shared".to_string()]);
        // Bob can attach to `shared`, but not `secret`.
        assert!(r.resolve_scoped(&bob, "", Some("shared")).is_some());
        assert!(r.resolve_scoped(&bob, "", Some("secret")).is_none());
        // Bob cannot use the agent (machine-less create/restore can't land).
        assert!(r.resolve_scoped(&bob, "laptop", None).is_none());
        assert!(r.resolve_scoped(&bob, "", None).is_none());
        // But the machine is listed so the UI can render where `shared` lives.
        assert_eq!(r.machines_for(&bob).len(), 1);

        // A third user with no grant sees nothing.
        let carol = Visibility::user("carol");
        assert!(r.all_sessions_for(&carol).is_empty());
        assert!(r.machines_for(&carol).is_empty());
    }

    // Sharing-back: an agent-grantee (bob) shares one of his sessions back to the
    // owner (alice); alice then sees exactly that one even with peek off.
    #[test]
    fn session_share_back_to_owner() {
        let r = Registry::new();
        let a = r.register_agent("agent-a", "alice", "laptop", "a.local", vec![], dummy_agent());
        a.set_sessions(vec![sess("claude-a"), sess("bee-1"), sess("bee-2")]);
        a.set_creator("bee-1", "bob");
        a.set_creator("bee-2", "bob");

        // Peek off, bob shares bee-1 back to alice.
        let alice = Visibility::from_rows(
            "alice".into(),
            vec![row("s3", "agent-a", "alice", "alice", "session", Some("bee-1"), false)],
        );
        let names: HashSet<String> = r.all_sessions_for(&alice).into_iter().map(|s| s.name).collect();
        assert_eq!(names, HashSet::from(["claude-a".into(), "bee-1".into()]), "owner sees own + shared-back, not bee-2");
        // Alice (owner) can attach to the shared-back session.
        assert!(r.resolve_scoped(&alice, "", Some("bee-1")).is_some());
    }

    #[tokio::test]
    async fn request_routes_command_to_agent_and_resolves_reply() {
        use cc_screen_protocol::hub::decode_frame;
        let r = Registry::new();
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(8);
        let a = r.register("alpha", "a", vec![], tx);

        // Issue a control op; it blocks awaiting the reply.
        let a2 = a.clone();
        let join = tokio::spawn(async move {
            a2.request(Cmd::Key { session: "claude-x".into(), key: "enter".into() }).await
        });

        // The "agent" receives the Command frame; reply by its req id.
        let frame = rx.recv().await.expect("a Command frame was sent");
        let (msg, _) = decode_frame::<HubMsg>(&frame).expect("decodes");
        let req = match msg {
            HubMsg::Command { req, cmd } => {
                assert!(matches!(cmd, Cmd::Key { .. }), "the routed cmd arrives intact");
                req
            }
            other => panic!("expected Command, got {other:?}"),
        };
        a.resolve_reply(req, CmdResult::Ok);
        assert_eq!(join.await.unwrap(), Ok(CmdResult::Ok));
    }

    #[tokio::test]
    async fn request_errors_when_agent_goes_offline() {
        let r = Registry::new();
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(8);
        let a = r.register("alpha", "a", vec![], tx);

        let a2 = a.clone();
        let join = tokio::spawn(async move { a2.request(Cmd::Restore).await });
        // Let the request register its pending slot, then drop the agent.
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.go_offline();
        // The pending reply sender was dropped → the request errors out.
        assert_eq!(join.await.unwrap(), Err(RequestErr::Timeout));
    }
}
