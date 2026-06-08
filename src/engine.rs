// The session engine — the tmux replacement. Each Session owns a PTY master for
// its whole lifetime (NOT per-WebSocket, unlike the Go `tmux attach` PTY): that
// is what lets input (key/paste/clip) work with no client attached, and what a
// WebSocket attaches to. A blocking reader thread pumps PTY output into two
// sinks: a server-side terminal emulator (render::Emulator — the authoritative
// screen + scrollback, serialized into a clean size-agnostic repaint on
// (re)attach) and a broadcast channel (live raw fan-out to attached clients).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, watch};

use crate::clip::ClipStore;
use crate::manifest;
use crate::render::Emulator;
use crate::tools::{self, Tool};

const BROADCAST_CAP: usize = 2048;
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;

/// How long a session must produce *no* PTY output before we call its agent
/// "waiting for input". The AI CLIs animate a sub-second spinner while they
/// work — Claude's elapsed-time counter ticks ~1×/s, codex/gemini similar, and
/// the spinner keeps moving even while a tool/sub-process runs — so a few
/// seconds of total quiet cleanly separates "still going" from "your turn".
/// Deliberately conservative: a false *busy* (we're a touch slow to flag a
/// finish) is harmless; a false *waiting* mid-task would be a wrong signal.
pub const IDLE_AFTER_SECS: u64 = 4;
/// Minimum output-producing work time before a busy→waiting edge is worth a
/// phone notification. Short answers should update the UI, not buzz a device.
pub const NOTIFY_MIN_WORK_SECS: u64 = 60;
/// Minimum time since the last client input before a busy→waiting edge can buzz.
/// This filters out PTY echo from the user's own typing and mid-run steering.
pub const NOTIFY_INPUT_QUIET_SECS: u64 = 60;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Pure predicate behind `Session::waiting`, split out so it's testable without
/// a real clock: the session has gone quiet for at least `IDLE_AFTER_SECS`.
fn is_waiting(last_activity: u64, now: u64) -> bool {
    now.saturating_sub(last_activity) >= IDLE_AFTER_SECS
}

/// Shared gate for "agent finished" push notifications. `busy_since == 0`
/// means we never observed a clean output-resumed-after-waiting transition, so
/// first-sight / startup-busy sessions are suppressed conservatively.
pub fn notification_eligible(busy_since: u64, last_input_at: u64, now: u64) -> bool {
    busy_since != 0
        && now.saturating_sub(busy_since) >= NOTIFY_MIN_WORK_SECS
        && now.saturating_sub(last_input_at) >= NOTIFY_INPUT_QUIET_SECS
}

struct SessionState {
    emu: Emulator,
    last_activity: u64,
    last_input_at: u64,
    busy_since: u64,
    cols: u16,
    rows: u16,
    // Per-attached-client requested sizes, keyed by connection id. The PTY is
    // sized to the MINIMUM cols/rows across these (tmux's `window-size smallest`
    // model). Why min, not last-writer: the tool (Claude/codex/…) renders with
    // *absolute* cursor-column positioning computed for the PTY width, so the
    // byte stream is width-locked — it only lays out correctly in a grid of that
    // exact width. Pinning the PTY to the narrowest client means that client
    // renders perfectly, and every wider client's columns all fit (no clamp /
    // pending-wrap), so they render the same content left-aligned with blank
    // space — also correct. Last-writer-wins instead let two clients (e.g. the
    // web PWA + the `ccs` TUI) of different widths fight and garble each other.
    client_sizes: HashMap<u64, (u16, u16)>,
    /// Bounded ring of recent raw operator input (drop-oldest), the single source
    /// for reconstructing "the last things the user asked" for the session summary
    /// (proposal 0022). Appended in `write_input()`; never sent raw — the
    /// candidacy path normalizes + redacts it first.
    input_ring: Vec<u8>,
    /// The cached LLM summary (headline/detail), produced by the hub (or the
    /// standalone fallback) and surfaced to every client via `SessionInfo`.
    summary: Option<crate::summary::Summary>,
    /// Content hash the cached `summary` describes (0 = none). A session is a
    /// summary *candidate* when its current content hash differs from this.
    summary_hash: u64,
    /// Content hash of the most recent in-flight `SummaryRequest` (0 = none).
    /// Stops a slow round-trip from re-firing each tick, and lets a stale result
    /// (hash no longer the latest requested) be dropped.
    requested_hash: u64,
}

/// Max bytes retained in the per-session input ring (drop-oldest). Generous
/// enough for a few recent submissions; bounded so long sessions don't grow.
const INPUT_RING_CAP: usize = 4096;

pub struct Session {
    pub name: String,   // full, e.g. claude-myproj
    pub tool: String,   // prefix, e.g. claude
    pub short: String,  // name minus "<prefix>-"
    pub launch_dir: String,
    pub pid: Option<u32>,
    // Session metadata kept for debugging / future use (not read yet).
    #[allow(dead_code)]
    pub cmd: String,
    #[allow(dead_code)]
    pub extra_dirs: Vec<String>,
    #[allow(dead_code)]
    pub created: u64,
    /// Whether this session launched YOLO — reported to clients as a badge.
    pub skip_permissions: bool,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    state: Mutex<SessionState>,
    tx: broadcast::Sender<Bytes>,
    // Flips to true the instant the child process exits, so attached WebSockets
    // close immediately instead of sitting on a frozen final frame until the
    // next /api/sessions poll unmounts the pane.
    closed: watch::Sender<bool>,
    // Hands out a unique id per attached client so `client_sizes` can track each
    // connection's requested size independently (and drop it on disconnect).
    next_client_id: AtomicU64,
}

impl Session {
    /// Spawn a tool under a fresh PTY. Returns the session handle plus the child
    /// process (the caller owns the wait/reap so it can update the registry).
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        tool: &Tool,
        short: &str,
        dir: &str,
        extra_dirs: Vec<String>,
        resume: bool,
        skip_permissions: bool,
        env_path: &str,
        clip_url: &str,
    ) -> anyhow::Result<(Arc<Session>, Box<dyn portable_pty::Child + Send + Sync>)> {
        let full = format!("{}-{}", tool.prefix, short);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: INIT_ROWS,
            cols: INIT_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let launch = tools::build_launch(tool, short, &extra_dirs, resume, skip_permissions);
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(&launch);
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("PATH", env_path);
        // The session name, so the clipboard shim can scope its image fetch with
        // `?session=` (see clip.rs) — a per-session slot prevents one session's
        // staged screenshot being served to another's paste.
        cmd.env("CCWEB_SESSION", &full);
        // Where the shim fetches this session's staged clipboard image from — this
        // very agent's bind. Decouples paste from the legacy Go server's config
        // dir, which the old shim was hardwired to (proposal 0007). Empty in tests
        // and for hub-only agents (no bind — those rely on CCWEB_CLIP_FILE below).
        if !clip_url.is_empty() {
            cmd.env("CCWEB_CLIP_URL", clip_url);
        }
        // The local drop file the shim can read even with no HTTP bind (hub-only).
        // Always set: it's the only source that works for a hub-only agent and a
        // harmless duplicate for a bound one. See clip.rs.
        if let Some(path) = crate::clip::session_clip_file(&full) {
            cmd.env("CCWEB_CLIP_FILE", path);
        }

        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave so the child is the sole holder of the slave side;
        // otherwise the master read never sees EOF when the child exits.
        drop(pair.slave);

        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (tx, _rx) = broadcast::channel::<Bytes>(BROADCAST_CAP);
        let (closed, _) = watch::channel(false);

        let now = now_secs();
        let state = SessionState {
            emu: Emulator::new(INIT_COLS, INIT_ROWS),
            last_activity: now,
            last_input_at: now,
            busy_since: 0,
            cols: INIT_COLS,
            rows: INIT_ROWS,
            client_sizes: HashMap::new(),
            input_ring: Vec::new(),
            summary: None,
            summary_hash: 0,
            requested_hash: 0,
        };

        let sess = Arc::new(Session {
            name: full,
            tool: tool.prefix.clone(),
            cmd: tool.cmd.clone(),
            short: short.to_string(),
            launch_dir: dir.to_string(),
            extra_dirs,
            created: now,
            skip_permissions,
            pid,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            state: Mutex::new(state),
            tx,
            closed,
            next_client_id: AtomicU64::new(0),
        });

        {
            let sess = sess.clone();
            std::thread::spawn(move || pump(sess, reader));
        }
        Ok((sess, child))
    }

    pub fn write_input(&self, data: &[u8]) {
        if !data.is_empty() {
            if let Ok(mut st) = self.state.lock() {
                st.last_input_at = now_secs();
                // Capture into the bounded input ring (drop-oldest). This is the
                // single choke point every input path funnels through, so one
                // append covers typed keys, named keys, paste, and Ctrl-combos.
                st.input_ring.extend_from_slice(data);
                if st.input_ring.len() > INPUT_RING_CAP {
                    let drop = st.input_ring.len() - INPUT_RING_CAP;
                    st.input_ring.drain(0..drop);
                }
            }
        }
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
    }

    /// Reconstruct the operator's recent typed submissions from the input ring
    /// (segmented + cleaned; see `summary::normalize_input`). Convenience accessor
    /// exercised by tests; `summary_extract` does the same under one lock.
    #[allow(dead_code)]
    pub fn recent_input(&self) -> Vec<String> {
        match self.state.lock() {
            Ok(st) => crate::summary::normalize_input(&st.input_ring),
            Err(_) => Vec::new(),
        }
    }

    /// A plain-text window of the last `max_lines` rows of the buffer (no ANSI),
    /// for the session-summary LLM context. Convenience accessor; `summary_extract`
    /// reads the same under one lock.
    #[allow(dead_code)]
    pub fn tail_text(&self, max_lines: usize) -> String {
        match self.state.lock() {
            Ok(st) => st.emu.tail_text(max_lines),
            Err(_) => String::new(),
        }
    }

    /// The current cached LLM summary, if any.
    pub fn summary(&self) -> Option<crate::summary::Summary> {
        self.state.lock().ok().and_then(|st| st.summary.clone())
    }

    /// Build the redacted summary extract for a candidacy check: the recent
    /// submissions, the terminal tail, and the content hash over both. Redaction
    /// happens here so nothing secret-shaped leaves the agent. Returns the hash
    /// plus the (redacted) inputs/tail ready for a `SummaryRequest`.
    pub fn summary_extract(&self, tail_lines: usize) -> (u64, Vec<String>, String) {
        let (inputs_raw, tail_raw) = match self.state.lock() {
            Ok(st) => (crate::summary::normalize_input(&st.input_ring), st.emu.tail_text(tail_lines)),
            Err(_) => (Vec::new(), String::new()),
        };
        let inputs: Vec<String> = inputs_raw.iter().map(|s| crate::summary::redact(s)).collect();
        let tail = crate::summary::redact(&tail_raw);
        let hash = crate::summary::content_hash(&inputs, &tail);
        (hash, inputs, tail)
    }

    /// Whether this session is a summary *candidate*: its current content differs
    /// from the cached summary's content AND isn't already the in-flight request.
    /// `hash` is the current content hash (from `summary_extract`).
    pub fn summary_candidate(&self, hash: u64) -> bool {
        match self.state.lock() {
            Ok(st) => hash != st.summary_hash && hash != st.requested_hash,
            Err(_) => false,
        }
    }

    /// Record that a `SummaryRequest` for `hash` is now in flight.
    pub fn mark_summary_requested(&self, hash: u64) {
        if let Ok(mut st) = self.state.lock() {
            st.requested_hash = hash;
        }
    }

    /// Store a returned summary. Drops it as stale (returns `false`) if `hash` is
    /// not the latest requested hash — the session changed again meanwhile, so a
    /// newer request is (or will be) in flight and this result would be a stale
    /// overwrite. On accept, the cache + `summary_hash` advance to `hash`.
    pub fn store_summary(&self, hash: u64, headline: String, detail: String) -> bool {
        if let Ok(mut st) = self.state.lock() {
            if hash != st.requested_hash {
                return false;
            }
            st.summary = Some(crate::summary::Summary { headline, detail });
            st.summary_hash = hash;
            true
        } else {
            false
        }
    }

    /// Low-level PTY + emulator resize. Prefer the per-client API
    /// (`register_client` / `resize_client` / `unregister_client`) over calling
    /// this directly — it does no min-size reconciliation, so a raw call would be
    /// overridden the next time any client's size changes.
    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
        if let Ok(mut st) = self.state.lock() {
            st.emu.resize(cols, rows);
            st.cols = cols;
            st.rows = rows;
        }
    }

    /// Register a freshly-attached client and return its connection id. The
    /// client starts with no size constraint (it sends its real size in a `"r"`
    /// frame right after attaching) so registering alone never resizes the PTY.
    pub fn register_client(&self) -> u64 {
        let id = self.next_client_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut st) = self.state.lock() {
            st.client_sizes.insert(id, (0, 0));
        }
        id
    }

    /// Record `client`'s requested size and re-pin the PTY to the minimum across
    /// all attached clients (see `client_sizes`). No-op if the min is unchanged.
    pub fn resize_client(&self, client: u64, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if let Some((c, r)) = self.reconcile(|sizes| {
            sizes.insert(client, (cols, rows));
        }) {
            self.resize(c, r);
        }
    }

    /// Drop a client (on disconnect) and re-pin the PTY to the new minimum. When
    /// the narrowest client leaves, the PTY grows back for those remaining.
    pub fn unregister_client(&self, client: u64) {
        if let Some((c, r)) = self.reconcile(|sizes| {
            sizes.remove(&client);
        }) {
            self.resize(c, r);
        }
    }

    /// Apply `mutate` to the client-size map under the state lock, then compute
    /// the new target PTY size as the per-axis minimum over clients that have
    /// reported a real size. Returns `Some((cols, rows))` only when that target
    /// differs from the current PTY size — i.e. when a `resize` is actually
    /// needed. Returns `None` while no client has reported yet (keeps the PTY at
    /// its current/initial size).
    fn reconcile<F: FnOnce(&mut HashMap<u64, (u16, u16)>)>(&self, mutate: F) -> Option<(u16, u16)> {
        let mut st = self.state.lock().ok()?;
        mutate(&mut st.client_sizes);
        let min = st
            .client_sizes
            .values()
            .filter(|&&(c, r)| c > 0 && r > 0)
            .copied()
            .reduce(|(ac, ar), (c, r)| (ac.min(c), ar.min(r)));
        match min {
            Some((c, r)) if (c, r) != (st.cols, st.rows) => Some((c, r)),
            _ => None,
        }
    }

    /// Current PTY/parser size `(cols, rows)`. Read on attach to decide whether a
    /// just-serialized snapshot is too wide for a narrower client (see
    /// `attach::attach_loop`); also drives the min-size tests.
    pub fn current_size(&self) -> (u16, u16) {
        let st = self.state.lock().unwrap();
        (st.cols, st.rows)
    }

    pub fn kill(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
    }

    /// Graceful end: type the agent's `/exit` + Enter (the AI CLIs quit on it).
    /// The child then exits 0, which the reaper treats as a clean exit.
    pub fn graceful_exit(&self) {
        self.write_input(b"/exit\r");
    }

    /// Subscribe to the live output stream AND capture the current repaint
    /// snapshot atomically, so no byte is both replayed and streamed (and none
    /// is missed). Relies on the pump processing+broadcasting under the same
    /// state lock. The snapshot is a clean, size-agnostic repaint of the
    /// emulator's scrollback + screen (RIS-prefixed) — NOT the raw byte history,
    /// whose size-locked redraws duplicated/staircased at mismatched client sizes.
    ///
    /// Used for the FIRST attach AND for every resync/refit afterwards: an
    /// already-attached client must call this (not a bare snapshot) and swap in
    /// the returned receiver, dropping its old one — otherwise a burst already
    /// folded into this snapshot but still queued in the old receiver gets
    /// repainted a second time (the duplicated-banner bug). See `attach::attach_loop`.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Bytes>) {
        let st = self.state.lock().unwrap();
        (st.emu.snapshot(), self.tx.subscribe())
    }

    /// Clear scrollback but keep the visible screen (tmux clear-history
    /// semantics): drop the emulator's history, then push a fresh repaint to
    /// every attached client. Broadcast under the lock to preserve the attach
    /// invariant.
    pub fn clear_history(&self) {
        let mut st = self.state.lock().unwrap();
        st.emu.clear_history();
        let payload = st.emu.snapshot();
        let _ = self.tx.send(Bytes::from(payload));
    }

    pub fn attached(&self) -> bool {
        self.tx.receiver_count() > 0
    }

    /// Receiver that fires when the child process exits (see `closed`).
    pub fn closed_rx(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    /// Signal that the child has exited — wakes attached WebSockets to close.
    pub fn mark_closed(&self) {
        let _ = self.closed.send(true);
    }

    pub fn last_activity(&self) -> u64 {
        self.state.lock().map(|s| s.last_activity).unwrap_or(0)
    }

    pub fn last_input_at(&self) -> u64 {
        self.state.lock().map(|s| s.last_input_at).unwrap_or(0)
    }

    pub fn busy_since(&self) -> u64 {
        self.state.lock().map(|s| s.busy_since).unwrap_or(0)
    }

    /// True when the agent has stopped streaming for `IDLE_AFTER_SECS` and is
    /// (almost always) waiting for input — the "your turn" signal the clients
    /// surface. A pure function of last-output time; see `is_waiting`.
    pub fn waiting(&self) -> bool {
        self.waiting_at(now_secs())
    }

    pub fn waiting_at(&self, now: u64) -> bool {
        is_waiting(self.last_activity(), now)
    }

    /// True when the current busy→waiting edge should produce a push
    /// notification. Callers still own edge detection; this is only the gate.
    pub fn notification_eligible_at(&self, now: u64) -> bool {
        self.state
            .lock()
            .map(|s| notification_eligible(s.busy_since, s.last_input_at, now))
            .unwrap_or(false)
    }

    pub fn preview(&self) -> String {
        match self.state.lock() {
            Ok(s) => s.emu.preview(),
            Err(_) => String::new(),
        }
    }

    /// The session's live working dir (the agent may have `cd`'d). Read from
    /// /proc, falling back to the launch dir — the analogue of tmux's
    /// #{pane_current_path}.
    pub fn live_cwd(&self) -> String {
        if let Some(pid) = self.pid {
            if let Ok(p) = std::fs::read_link(format!("/proc/{pid}/cwd")) {
                return p.to_string_lossy().into_owned();
            }
        }
        self.launch_dir.clone()
    }
}

/// Output pump: blocking-read the PTY and fan out to the emulator + broadcast.
/// CRITICAL: the broadcast send happens INSIDE the state lock, so a concurrent
/// `attach()` (which snapshots + subscribes under the same lock) can never see a
/// byte both in its snapshot and its live stream.
fn pump(sess: Arc<Session>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 32 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break, // EOF — child exited
            Ok(n) => {
                let chunk = &buf[..n];
                if let Ok(mut st) = sess.state.lock() {
                    let now = now_secs();
                    if is_waiting(st.last_activity, now) {
                        st.busy_since = now;
                    }
                    st.emu.process(chunk);
                    st.last_activity = now;
                    let _ = sess.tx.send(Bytes::copy_from_slice(chunk));
                }
            }
            Err(_) => break,
        }
    }
    // PTY hit EOF (the child's slave side closed) → tell attached clients.
    sess.mark_closed();
}

// ── Application state ─────────────────────────────────────────────────────────
pub struct Inner {
    pub tools: Vec<Tool>,
    pub registry: Mutex<HashMap<String, Arc<Session>>>,
    pub env_path: String,
    /// Loopback base URL exported to each session as `CCWEB_CLIP_URL` so the
    /// clipboard shim fetches staged images from THIS agent (see clip.rs). Empty
    /// in tests, in which case the env var is left unset.
    pub clip_url: String,
    pub config_dir: PathBuf,
    pub home: PathBuf,
    /// This agent's machine identity (hostname / `--machine-id`). Surfaced on
    /// `/api/session/root` so a direct client can name the box without a hub.
    pub machine_id: String,
    pub clip: ClipStore,
    pub watcher: crate::watch::Watcher,
    /// Web Push: VAPID keys + device subscriptions + the "agent finished" sender.
    pub push: crate::push::Push,
    /// Opt-in auth gate (password / API token). No-op when unconfigured.
    pub auth: crate::auth::Auth,
    /// Origin/Host validation policy (anti cross-origin / DNS-rebinding). Enforced
    /// independent of the auth gate; see `auth::require_auth`.
    pub origin: cc_screen_auth::OriginPolicy,
    /// Login attempt throttle (per-source backoff/lockout).
    pub login_throttle: cc_screen_auth::LoginThrottle,
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

impl AppState {
    pub fn new(
        tools: Vec<Tool>,
        env_path: String,
        clip_url: String,
        config_dir: PathBuf,
        home: PathBuf,
        machine_id: String,
        auth: crate::auth::Auth,
        origin: cc_screen_auth::OriginPolicy,
    ) -> AppState {
        AppState {
            inner: Arc::new(Inner {
                tools,
                registry: Mutex::new(HashMap::new()),
                env_path,
                clip_url,
                push: crate::push::Push::new(&config_dir),
                config_dir,
                watcher: crate::watch::Watcher::new(home.clone()),
                home,
                machine_id,
                clip: ClipStore::default(),
                auth,
                origin,
                login_throttle: cc_screen_auth::LoginThrottle::new(),
            }),
        }
    }

    pub fn find_tool(&self, key: &str) -> Option<Tool> {
        self.inner.tools.iter().find(|t| t.cmd == key || t.prefix == key).cloned()
    }

    pub fn get(&self, name: &str) -> Option<Arc<Session>> {
        self.inner.registry.lock().unwrap().get(name).cloned()
    }

    pub fn list(&self) -> Vec<Arc<Session>> {
        let mut v: Vec<Arc<Session>> = self.inner.registry.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Create + register a session, spawning a reaper thread that drops it from
    /// the registry when the child exits.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        tool: &Tool,
        name: &str,
        dir: &str,
        extra_dirs: Vec<String>,
        resume: bool,
        skip_permissions: bool,
    ) -> anyhow::Result<String> {
        let short = tools::sanitize_name(name);
        if short.is_empty() {
            anyhow::bail!("invalid name");
        }
        let full = format!("{}-{}", tool.prefix, short);
        {
            let reg = self.inner.registry.lock().unwrap();
            if reg.contains_key(&full) {
                anyhow::bail!("session already exists: {full}");
            }
        }
        let (sess, mut child) = Session::spawn(
            tool,
            &short,
            dir,
            extra_dirs.clone(),
            resume,
            skip_permissions,
            &self.inner.env_path,
            &self.inner.clip_url,
        )?;
        self.inner.registry.lock().unwrap().insert(full.clone(), sess.clone());

        // Record for resume-after-restart (best-effort). The launch policy is
        // persisted so a redeploy relaunches the session under the same policy.
        manifest::record(
            &self.inner.config_dir,
            manifest::Entry {
                session: full.clone(),
                cmd: tool.cmd.clone(),
                prefix: tool.prefix.clone(),
                short: short.clone(),
                dir: dir.to_string(),
                extra_dirs,
                created_at: now_secs() as i64,
                skip_permissions,
            },
        );

        let inner = self.inner.clone();
        let key = full.clone();
        let sess_reaper = sess;
        std::thread::spawn(move || {
            let status = child.wait();
            sess_reaper.mark_closed(); // close attached WS the instant the child exits
            inner.registry.lock().unwrap().remove(&key);
            // A clean exit (the user typed /exit; status 0) is deliberate → drop
            // it from the manifest so it isn't restored. A crash/signal — and a
            // backend redeploy, where this thread never even runs — leaves the
            // entry in place, so auto-restore brings it back. (A web delete has
            // already forgotten it via the handler; forget is idempotent.)
            if matches!(status, Ok(s) if s.success()) {
                manifest::forget(&inner.config_dir, &key);
            }
        });
        Ok(full)
    }

    /// Bring back every recorded-but-not-live session, resuming its conversation.
    /// Idempotent; used by POST /api/sessions/restore and at startup.
    pub fn restore_all(&self) -> (Vec<String>, HashMap<String, String>) {
        let live: std::collections::HashSet<String> =
            self.inner.registry.lock().unwrap().keys().cloned().collect();
        let mut restored = Vec::new();
        let mut failed = HashMap::new();
        for e in manifest::entries(&self.inner.config_dir) {
            if live.contains(&e.session) {
                continue;
            }
            let Some(tool) = self.find_tool(&e.prefix).or_else(|| self.find_tool(&e.cmd)) else {
                continue;
            };
            if !std::path::Path::new(&e.dir).is_dir() {
                continue;
            }
            match self.create(
                &tool,
                &e.short,
                &e.dir,
                e.extra_dirs.clone(),
                true,
                e.skip_permissions,
            ) {
                Ok(name) => restored.push(name),
                Err(err) => {
                    failed.insert(e.session.clone(), err.to_string());
                }
            }
        }
        (restored, failed)
    }

    /// Manifest entries not currently live whose tool + dir still exist.
    pub fn restorable(&self) -> Vec<manifest::Entry> {
        let live: std::collections::HashSet<String> =
            self.inner.registry.lock().unwrap().keys().cloned().collect();
        manifest::entries(&self.inner.config_dir)
            .into_iter()
            .filter(|e| !live.contains(&e.session))
            .filter(|e| self.find_tool(&e.prefix).or_else(|| self.find_tool(&e.cmd)).is_some())
            .filter(|e| std::path::Path::new(&e.dir).is_dir())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_tool(tmpl: &str) -> Tool {
        Tool {
            cmd: "tt".into(),
            prefix: "shell".into(),
            tmpl: tmpl.into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
        }
    }

    #[test]
    fn waiting_after_idle_threshold() {
        // Fresh output → working; quiet past the threshold → waiting. Boundary at
        // exactly IDLE_AFTER_SECS counts as waiting (>=).
        let now = 1_000_000;
        assert!(!is_waiting(now, now), "just produced output → working");
        assert!(!is_waiting(now - (IDLE_AFTER_SECS - 1), now), "one second short → still working");
        assert!(is_waiting(now - IDLE_AFTER_SECS, now), "quiet for the threshold → waiting");
        assert!(is_waiting(now - 3600, now), "quiet for an hour → waiting");
        // A clock that went backwards must not underflow into "waiting".
        assert!(!is_waiting(now + 5, now), "future last-activity → working, not underflow");
    }

    #[test]
    fn notification_gate_requires_work_and_input_quiet() {
        let now = 1_000_000;
        assert!(
            notification_eligible(
                now - NOTIFY_MIN_WORK_SECS,
                now - NOTIFY_INPUT_QUIET_SECS,
                now
            ),
            "long work with no recent input should notify"
        );
        assert!(
            !notification_eligible(0, now - NOTIFY_INPUT_QUIET_SECS, now),
            "unknown busy start suppresses first-sight sessions"
        );
        assert!(
            !notification_eligible(now - 10, now - NOTIFY_INPUT_QUIET_SECS, now),
            "quick replies should not notify"
        );
        assert!(
            !notification_eligible(now - NOTIFY_MIN_WORK_SECS, now - 4, now),
            "recent user input should suppress echoed typing and mid-run steering"
        );
        assert!(
            !notification_eligible(now + 10, now + 10, now),
            "clock skew should not underflow into a notification"
        );
    }

    // End-to-end over a real PTY + real time: a session streaming output reads as
    // "working" (waiting=false), and once its output stops for IDLE_AFTER_SECS it
    // flips to "waiting". This is the live integration behind the pure
    // `waiting_after_idle_threshold` test above — it exercises the pump thread
    // stamping `last_activity` on every read.
    #[tokio::test]
    async fn waiting_flips_with_live_output() {
        let tmp = std::env::temp_dir().join(format!("ccr-wait-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Stream a byte every 200ms for ~2s, then go quiet.
        let tool = shell_tool("for i in $(seq 1 10); do printf x; sleep 0.2; done; sleep 6");
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        let sess = state.get(&name).unwrap();

        // ~1s in: mid-stream, output landed well under IDLE_AFTER_SECS ago.
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        assert!(!sess.waiting(), "streaming output should read as working");

        // Output stops at ~2s; wait past IDLE_AFTER_SECS of quiet (2s + 5s).
        tokio::time::sleep(std::time::Duration::from_millis(6000)).await;
        assert!(sess.waiting(), "after a quiet spell it should read as waiting");

        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Spawns a real PTY (no tmux) and asserts the engine sees its output through
    // the vt100 preview and the reattach snapshot — the core M1 path.
    #[tokio::test]
    async fn spawn_preview_and_snapshot() {
        let tmp = std::env::temp_dir().join(format!("ccr-etest-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tool = shell_tool("printf READY_MARK; sleep 3");
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        assert_eq!(name, "shell-t");

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let list = state.list();
        assert_eq!(list.len(), 1);
        assert!(
            list[0].preview().contains("READY_MARK"),
            "preview was {:?}",
            list[0].preview()
        );
        let (snap, _rx) = list[0].attach();
        let snap = String::from_utf8_lossy(&snap);
        assert!(snap.starts_with('\u{1b}')); // RIS reset prefix
        assert!(snap.contains("READY_MARK"));
        list[0].kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // A spawned session must carry the clipboard contract env (proposal 0007):
    // `CCWEB_CLIP_URL` (where the shim fetches a staged paste) and `CCWEB_SESSION`
    // (so it scopes the fetch). We prove it end-to-end by having the child echo
    // both and reading them back off the engine's preview.
    #[tokio::test]
    async fn session_exports_clip_url_and_name() {
        let tmp = std::env::temp_dir().join(format!("ccr-clipenv-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // One field per line (a single line would wrap past the 80-col preview).
        let tool = shell_tool(
            "printf 'CLIP[%s]\\nSES[%s]\\nFILE[%s]\\n' \"$CCWEB_CLIP_URL\" \"$CCWEB_SESSION\" \"$CCWEB_CLIP_FILE\"; sleep 3",
        );
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            "http://127.0.0.1:8839".into(), // non-empty → exported to the child
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Read the full snapshot (not the one-line preview) so all three lines show.
        let (snap, _rx) = state.list()[0].attach();
        let snap = String::from_utf8_lossy(&snap);
        assert!(snap.contains("CLIP[http://127.0.0.1:8839]"), "snap was {snap:?}");
        assert!(snap.contains(&format!("SES[{name}]")), "snap was {snap:?}");
        // The local drop-file path is always exported (the only source that works
        // for a hub-only agent), scoped to this session.
        assert!(
            snap.contains(&format!("/cc-screen/clip/{name}.png]")),
            "snap was {snap:?}"
        );

        state.get(&name).unwrap().kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Regression for the duplicated-banner bug: a resync/refit (`attach_loop`'s
    // Lagged + narrow-first-resize paths) MUST re-subscribe via `attach()`, not
    // snapshot a stale receiver. A burst broadcast after the first attach but
    // before the resync is folded into the resync snapshot — the fresh receiver
    // must NOT also deliver it, or the client repaints it twice.
    #[tokio::test]
    async fn resync_attach_does_not_replay_snapshotted_bytes() {
        let tmp = std::env::temp_dir().join(format!("ccr-resync-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Delay the burst so it is broadcast AFTER the first attach subscribes —
        // that is the byte that used to get repainted twice.
        let tool = shell_tool("sleep 1; printf BURST_MARK; sleep 5");
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        let sess = state.get(&name).unwrap();

        // First attach (the live client) BEFORE the burst. Leave rx1 UNDRAINED —
        // mimics a loop parked on a slow `out.send` while the pump broadcasts.
        let (_snap1, mut rx1) = sess.attach();

        // Wait until the burst is processed into the emulator (and thus broadcast).
        for _ in 0..200 {
            if sess.preview().contains("BURST_MARK") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(sess.preview().contains("BURST_MARK"), "burst reached the emulator");

        // The stale receiver HAS the burst queued — the bytes that used to be
        // repainted a second time.
        let queued = rx1.try_recv().expect("burst was queued in the original receiver");
        assert!(String::from_utf8_lossy(&queued).contains("BURST_MARK"));

        // The resync/refit: snapshot + fresh subscription, atomically.
        let (snap2, mut rx2) = sess.attach();
        assert!(
            String::from_utf8_lossy(&snap2).contains("BURST_MARK"),
            "the resync snapshot already contains the burst"
        );

        // The fresh receiver subscribed AFTER the snapshot point, so it must be
        // empty of the already-snapshotted burst.
        match rx2.try_recv() {
            Err(broadcast::error::TryRecvError::Empty) => {}
            Ok(b) => panic!(
                "fresh receiver replayed snapshotted bytes: {:?}",
                String::from_utf8_lossy(&b)
            ),
            Err(e) => panic!("unexpected receiver state: {e:?}"),
        }

        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The input ring captures every keystroke through write_input, and the
    // candidacy/store gate behaves: an unchanged hash is not a candidate, and a
    // result whose hash isn't the latest requested is dropped as stale.
    #[tokio::test]
    async fn summary_capture_candidacy_and_stale_drop() {
        let tmp = std::env::temp_dir().join(format!("ccr-sum-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tool = shell_tool("sleep 5");
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        let sess = state.get(&name).unwrap();

        // Typed input is captured + reconstructed.
        sess.write_input(b"fix the auth bug\r");
        sess.write_input(b"y\r");
        assert_eq!(sess.recent_input(), vec!["fix the auth bug", "y"]);

        // First extract → a candidate (no cached summary yet).
        let (hash, inputs, _tail) = sess.summary_extract(200);
        assert!(sess.summary_candidate(hash), "changed content with no summary is a candidate");
        assert!(inputs.iter().any(|s| s == "fix the auth bug"));

        // Mark it in flight; the same hash is no longer a candidate.
        sess.mark_summary_requested(hash);
        assert!(!sess.summary_candidate(hash), "in-flight hash isn't re-fired");

        // A stale result (some other hash) is dropped.
        assert!(!sess.store_summary(hash.wrapping_add(1), "h".into(), "d".into()));
        assert!(sess.summary().is_none(), "stale result didn't overwrite");

        // The matching result is stored and clears candidacy.
        assert!(sess.store_summary(hash, "Waiting".into(), "It is paused.".into()));
        let s = sess.summary().expect("summary cached");
        assert_eq!(s.headline, "Waiting");
        assert!(!sess.summary_candidate(hash), "after storing, same content isn't a candidate");

        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // The PTY follows the per-axis minimum across attached clients, so the
    // narrowest client's width is what the tool renders for (and what every
    // client therefore renders cleanly).
    #[tokio::test]
    async fn pty_pins_to_min_client_size() {
        let tmp = std::env::temp_dir().join(format!("ccr-size-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tool = shell_tool("sleep 3");
        let state = AppState::new(
            vec![tool.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        let sess = state.get(&name).unwrap();

        // No client has reported a size yet → PTY stays at its init size.
        assert_eq!(sess.current_size(), (INIT_COLS, INIT_ROWS));

        let a = sess.register_client();
        let b = sess.register_client();
        // Registering alone carries no size constraint.
        assert_eq!(sess.current_size(), (INIT_COLS, INIT_ROWS));

        // One known size → the PTY adopts it.
        sess.resize_client(a, 100, 40);
        assert_eq!(sess.current_size(), (100, 40));

        // A second, narrower client pulls the PTY down to the per-axis min.
        sess.resize_client(b, 60, 30);
        assert_eq!(sess.current_size(), (60, 30));

        // The wide client growing further can't widen the PTY past the narrow one.
        sess.resize_client(a, 120, 50);
        assert_eq!(sess.current_size(), (60, 30));

        // The narrow client detaches → the PTY grows back for the one that's left.
        sess.unregister_client(b);
        assert_eq!(sess.current_size(), (120, 50));

        sess.kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
