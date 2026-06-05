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

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Pure predicate behind `Session::waiting`, split out so it's testable without
/// a real clock: the session has gone quiet for at least `IDLE_AFTER_SECS`.
fn is_waiting(last_activity: u64, now: u64) -> bool {
    now.saturating_sub(last_activity) >= IDLE_AFTER_SECS
}

struct SessionState {
    emu: Emulator,
    last_activity: u64,
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
}

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
    fn spawn(
        tool: &Tool,
        short: &str,
        dir: &str,
        extra_dirs: Vec<String>,
        resume: bool,
        env_path: &str,
    ) -> anyhow::Result<(Arc<Session>, Box<dyn portable_pty::Child + Send + Sync>)> {
        let full = format!("{}-{}", tool.prefix, short);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: INIT_ROWS,
            cols: INIT_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let launch = tools::build_launch(tool, short, &extra_dirs, resume);
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(&launch);
        cmd.cwd(dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("PATH", env_path);

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

        let state = SessionState {
            emu: Emulator::new(INIT_COLS, INIT_ROWS),
            last_activity: now_secs(),
            cols: INIT_COLS,
            rows: INIT_ROWS,
            client_sizes: HashMap::new(),
        };

        let sess = Arc::new(Session {
            name: full,
            tool: tool.prefix.clone(),
            cmd: tool.cmd.clone(),
            short: short.to_string(),
            launch_dir: dir.to_string(),
            extra_dirs,
            created: now_secs(),
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
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
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

    /// True when the agent has stopped streaming for `IDLE_AFTER_SECS` and is
    /// (almost always) waiting for input — the "your turn" signal the clients
    /// surface. A pure function of last-output time; see `is_waiting`.
    pub fn waiting(&self) -> bool {
        is_waiting(self.last_activity(), now_secs())
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
                    st.emu.process(chunk);
                    st.last_activity = now_secs();
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
}

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

impl AppState {
    pub fn new(
        tools: Vec<Tool>,
        env_path: String,
        config_dir: PathBuf,
        home: PathBuf,
        machine_id: String,
        auth: crate::auth::Auth,
    ) -> AppState {
        AppState {
            inner: Arc::new(Inner {
                tools,
                registry: Mutex::new(HashMap::new()),
                env_path,
                push: crate::push::Push::new(&config_dir),
                config_dir,
                watcher: crate::watch::Watcher::new(home.clone()),
                home,
                machine_id,
                clip: ClipStore::default(),
                auth,
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
    pub fn create(
        &self,
        tool: &Tool,
        name: &str,
        dir: &str,
        extra_dirs: Vec<String>,
        resume: bool,
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
        let (sess, mut child) =
            Session::spawn(tool, &short, dir, extra_dirs.clone(), resume, &self.inner.env_path)?;
        self.inner.registry.lock().unwrap().insert(full.clone(), sess.clone());

        // Record for resume-after-restart (best-effort).
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
            match self.create(&tool, &e.short, &e.dir, e.extra_dirs.clone(), true) {
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
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
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
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
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
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
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
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
        );
        let name = state.create(&tool, "t", &tmp.to_string_lossy(), vec![], false).unwrap();
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
