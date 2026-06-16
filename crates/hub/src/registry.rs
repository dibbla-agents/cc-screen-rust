//! The registry of connected agents and the live relay routing.
//!
//! Each agent is an [`AgentConn`] shared as `Arc`: the uplink server (which owns
//! the agent WS) and the client bridges (which own browser WSes) both hold it.
//! `to_agent` funnels encoded `HubMsg` frames to the agent; `channels` maps each
//! attached client's [`ChannelId`] to a sink toward its browser WS. An agent that
//! drops its uplink is *greyed* (kept, marked offline, its last session list
//! retained) rather than vanishing.

use std::collections::HashMap;
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

    /// Register a freshly-connected agent (online), returning the shared conn.
    /// Replaces any prior entry for this machine (a reconnect) but carries over the
    /// last-known session list so the UI doesn't blink empty before the first poll.
    pub fn register(
        &self,
        machine_id: &str,
        hostname: &str,
        tools: Vec<ToolInfo>,
        to_agent: mpsc::Sender<Vec<u8>>,
    ) -> Arc<AgentConn> {
        let mut g = self.inner.lock().unwrap();
        let carried = g.get(machine_id).map(|c| c.sessions_tagged_raw()).unwrap_or_default();
        let conn = Arc::new(AgentConn {
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
        });
        g.insert(machine_id.to_string(), conn.clone());
        conn
    }

    pub fn get(&self, machine_id: &str) -> Option<Arc<AgentConn>> {
        self.inner.lock().unwrap().get(machine_id).cloned()
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
        if !machine.is_empty() {
            return self.get(machine).filter(|a| a.online());
        }
        let g = self.inner.lock().unwrap();
        let online: Vec<&Arc<AgentConn>> = g.values().filter(|a| a.online()).collect();
        match session {
            Some(name) => {
                let mut owners = online.into_iter().filter(|a| a.owns(name));
                let first = owners.next()?;
                // Ambiguous if a second machine also has a session by that name.
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

    pub fn is_online(&self, machine_id: &str) -> bool {
        self.get(machine_id).is_some_and(|c| c.online())
    }

    /// Union of every agent's sessions, each machine-tagged, sorted by
    /// `(machine, name)` so identical names on different machines never collide.
    pub fn all_sessions(&self) -> Vec<SessionInfo> {
        let g = self.inner.lock().unwrap();
        let mut out: Vec<SessionInfo> = g.values().flat_map(|c| c.sessions_tagged()).collect();
        out.sort_by(|a, b| (&a.machine, &a.name).cmp(&(&b.machine, &b.name)));
        out
    }

    /// The machine list for the picker / offline greying, sorted by id.
    pub fn machines(&self) -> Vec<MachineInfo> {
        let g = self.inner.lock().unwrap();
        let mut v: Vec<MachineInfo> = g
            .values()
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
