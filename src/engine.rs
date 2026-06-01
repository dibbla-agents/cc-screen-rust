// The session engine — the tmux replacement. Each Session owns a PTY master for
// its whole lifetime (NOT per-WebSocket, unlike the Go `tmux attach` PTY): that
// is what lets input (key/paste/clip) work with no client attached, and what a
// WebSocket attaches to. A blocking reader thread pumps PTY output into three
// sinks: a vt100 parser (authoritative screen → preview), a bounded raw-byte
// ring (replayed on (re)attach so a reconnecting xterm.js repaints correctly),
// and a broadcast channel (live fan-out to attached clients).

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use cc_screen_protocol::SNAPSHOT_RESET;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, watch};

use crate::clip::ClipStore;
use crate::manifest;
use crate::tools::{self, Tool};

const RING_CAP: usize = 768 * 1024; // ~768 KB raw-output replay buffer per session
const SCROLLBACK: usize = 5000;
const BROADCAST_CAP: usize = 2048;
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

struct SessionState {
    parser: vt100::Parser,
    ring: VecDeque<u8>,
    last_activity: u64,
    cols: u16,
    rows: u16,
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
            parser: vt100::Parser::new(INIT_ROWS, INIT_COLS, SCROLLBACK),
            ring: VecDeque::new(),
            last_activity: now_secs(),
            cols: INIT_COLS,
            rows: INIT_ROWS,
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

    pub fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
        if let Ok(mut st) = self.state.lock() {
            st.parser.set_size(rows, cols);
            st.cols = cols;
            st.rows = rows;
        }
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
    /// is missed). Relies on the pump broadcasting under the same state lock.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<Bytes>) {
        let st = self.state.lock().unwrap();
        let mut snap = Vec::with_capacity(st.ring.len() + 2);
        snap.extend_from_slice(SNAPSHOT_RESET); // RIS: full reset so a fresh emulator repaints clean
        snap.extend(st.ring.iter().copied());
        (snap, self.tx.subscribe())
    }

    /// Standalone repaint snapshot (used to resync a lagged client).
    pub fn snapshot(&self) -> Vec<u8> {
        let st = self.state.lock().unwrap();
        let mut snap = Vec::with_capacity(st.ring.len() + 2);
        snap.extend_from_slice(SNAPSHOT_RESET);
        snap.extend(st.ring.iter().copied());
        snap
    }

    /// Clear scrollback but keep the visible screen (tmux clear-history
    /// semantics): reset the ring to just a repaint of the current screen and
    /// push that to every attached client. Broadcast under the lock to preserve
    /// the attach invariant.
    pub fn clear_history(&self) {
        let mut st = self.state.lock().unwrap();
        let screen = st.parser.screen().contents_formatted();
        st.ring.clear();
        st.ring.extend(screen.iter().copied());
        let mut payload = Vec::with_capacity(screen.len() + 2);
        payload.extend_from_slice(SNAPSHOT_RESET);
        payload.extend_from_slice(&screen);
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

    pub fn preview(&self) -> String {
        let st = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let contents = st.parser.screen().contents();
        for line in contents.lines().rev() {
            let t = line.trim();
            if !t.is_empty() {
                if t.chars().count() > 120 {
                    return t.chars().take(120).collect();
                }
                return t.to_string();
            }
        }
        String::new()
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

/// Output pump: blocking-read the PTY and fan out to vt100 + ring + broadcast.
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
                    st.parser.process(chunk);
                    st.ring.extend(chunk.iter().copied());
                    let over = st.ring.len().saturating_sub(RING_CAP);
                    if over > 0 {
                        st.ring.drain(0..over);
                    }
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
    pub clip: ClipStore,
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
    ) -> AppState {
        AppState {
            inner: Arc::new(Inner {
                tools,
                registry: Mutex::new(HashMap::new()),
                env_path,
                config_dir,
                home,
                clip: ClipStore::default(),
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
}
