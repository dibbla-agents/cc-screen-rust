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
use cc_screen_protocol::SessionRestartStatus;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tokio::sync::{broadcast, watch};

use crate::clip::ClipStore;
use crate::manifest;
use crate::render::Emulator;
use crate::tools::{self, Tool};

const BROADCAST_CAP: usize = 2048;
const INIT_COLS: u16 = 80;
const INIT_ROWS: u16 = 24;

/// How long a restarted session (proposal 0049) gets to quit after `/exit`
/// before we escalate to SIGKILL, and how long the kill itself gets. Generous
/// enough for a CLI to flush its transcript, short enough that a stuck session
/// doesn't stall the rest of the job.
const GRACEFUL_STOP: std::time::Duration = std::time::Duration::from_secs(10);
const FORCED_STOP: std::time::Duration = std::time::Duration::from_secs(5);

/// Input-gated busy model (proposal 0024). "Working" is **armed by a user submit**
/// (Enter) and **sustained by output**, not armed by output. A submit opens a work
/// window (`busy_until = now + WORK_GRACE_SECS`); each output burst pushes the
/// window out while it's still open; after this many seconds of output-silence the
/// session flips to "waiting" (ready). Because only a submit opens the window,
/// cosmetic output — a focus/resize repaint on attach, the cursor, the spinner —
/// never makes a session read busy. The window is generous enough to bridge a
/// turn's mid-task silent gaps (model thinking / a tool running, ~10–16 s) so a
/// genuinely-working session doesn't false-flip to ready between its output bursts;
/// since focus no longer arms busy, this length carries no focus penalty.
pub const WORK_GRACE_SECS: u64 = 8;
/// Minimum output-producing work time before a busy→waiting edge is worth a
/// phone notification. Short answers should update the UI, not buzz a device.
pub const NOTIFY_MIN_WORK_SECS: u64 = 60;
/// Minimum time since the last client input before a busy→waiting edge can buzz.
/// This filters out PTY echo from the user's own typing and mid-run steering.
pub const NOTIFY_INPUT_QUIET_SECS: u64 = 60;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Pure predicate behind `Session::waiting`, split out so it's testable without a
/// real clock: a session is "working" while its submit-armed window is still open
/// (`now < busy_until`); once the deadline passes it reads as waiting/ready. The
/// window is opened in `write_input` (on a submit) and extended in `pump` (on
/// output that lands while it's open) — see proposal 0024. `busy_until == 0` (never
/// armed) reads as waiting.
fn is_working(busy_until: u64, now: u64) -> bool {
    now < busy_until
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Detect a user *submit* in a chunk of input bytes — a bare carriage return that
/// arms the busy window (proposal 0024). A submit is a `\r` (0x0D) that is **not**
/// inside a bracketed-paste span (`\e[200~ … \e[201~`, so a pasted newline doesn't
/// arm) and **not** immediately preceded by `ESC` (so a Shift-Enter / `\e\r`-style
/// newline encoding doesn't arm). `in_paste` carries the bracketed-paste state
/// across calls, since a paste may span several `write_input`s. A trailing `\r`
/// *after* the closing marker — as `wrap_bracketed_paste(text, enter=true)` emits —
/// correctly counts as a submit.
fn scan_for_submit(data: &[u8], in_paste: &mut bool) -> bool {
    let mut submit = false;
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(PASTE_START) {
            *in_paste = true;
            i += PASTE_START.len();
            continue;
        }
        if data[i..].starts_with(PASTE_END) {
            *in_paste = false;
            i += PASTE_END.len();
            continue;
        }
        if !*in_paste && data[i] == b'\r' && !(i > 0 && data[i - 1] == 0x1b) {
            submit = true;
        }
        i += 1;
    }
    submit
}

/// Shared gate for "agent finished" push notifications. `busy_since == 0` means
/// the session has never been submitted to (no turn yet), so first-sight /
/// startup sessions are suppressed conservatively; otherwise the turn must have
/// run ≥ `NOTIFY_MIN_WORK_SECS` and the user must have been quiet ≥
/// `NOTIFY_INPUT_QUIET_SECS`. Under 0024 `busy_since` is the submit (turn-start)
/// time, so "the turn ran long enough" is measured from when you hit Enter.
pub fn notification_eligible(busy_since: u64, last_input_at: u64, now: u64) -> bool {
    busy_since != 0
        && now.saturating_sub(busy_since) >= NOTIFY_MIN_WORK_SECS
        && now.saturating_sub(last_input_at) >= NOTIFY_INPUT_QUIET_SECS
}

struct SessionState {
    emu: Emulator,
    last_activity: u64,
    last_input_at: u64,
    /// Turn start (Unix secs): the time of the submit that armed the current/last
    /// busy window. 0 = never submitted to. Surfaced as `SessionInfo.busy_since`
    /// and used as the "working for N" timer anchor (proposal 0024).
    busy_since: u64,
    /// Busy-window deadline (Unix secs): the session reads as "working" while
    /// `now < busy_until`. Opened by a submit in `write_input`, extended by output
    /// in `pump` while still open; cosmetic output after it lapses does NOT reopen
    /// it (only a submit does). 0 = never armed (proposal 0024).
    busy_until: u64,
    /// Whether the input stream is currently inside a bracketed-paste span — so a
    /// `\r` within a paste isn't mistaken for a submit (carries across calls).
    in_bracketed_paste: bool,
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
    /// How a pasted clipboard image is delivered to this PTY (proposal 0066).
    /// Copied immutably from the resolved tool at spawn/restore; never client
    /// input. See `tools::ImagePasteStrategy` and `clip.rs`.
    pub image_paste: tools::ImagePasteStrategy,
    /// Operator-chosen mark colour (proposal 0029): a curated palette token, or
    /// `None` when unmarked. Mirrored on the live session for a lowest-latency
    /// read in `session_list`; the authoritative copy persists in the manifest.
    color: Mutex<Option<String>>,
    /// Operator-chosen display label (proposal 0035): free text shown in place of
    /// `short` wherever the session is named, or `None` for no label. Display-only
    /// — identity (`name`/`short`) is untouched. Mirrored on the live session for a
    /// lowest-latency read; the authoritative copy persists in the manifest.
    label: Mutex<Option<String>>,
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
        // The wrapping interpreter is platform-specific (`/bin/sh -c` on Unix,
        // `cmd.exe /C` on Windows); `native_pty_system()` already gave us a ConPTY
        // on Windows, so only the command wrapper differs. See tools::launch_shell.
        let (program, pre_args) = tools::launch_shell();
        let mut cmd = CommandBuilder::new(program);
        for a in pre_args {
            cmd.arg(a);
        }
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
            busy_until: 0,
            in_bracketed_paste: false,
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
            image_paste: tool.image_paste,
            color: Mutex::new(None),
            label: Mutex::new(None),
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
        let _ = self.write_input_checked(data);
    }

    /// Fallible input write (0066): returns how many bytes actually reached
    /// the PTY writer, plus the error that stopped it (if any). `written == 0`
    /// with an error is a *proven* zero-byte failure — the only case a caller
    /// may treat as "the assistant saw nothing" and roll back; a partial count
    /// is ambiguous delivery.
    pub fn write_input_checked(&self, data: &[u8]) -> (usize, Option<std::io::Error>) {
        if !data.is_empty() {
            if let Ok(mut st) = self.state.lock() {
                let now = now_secs();
                st.last_input_at = now;
                // A user *submit* (Enter) arms the busy window that output then
                // sustains — this is what makes "busy" mean "I gave the agent a
                // task," not "the terminal repainted." Detected as a bare CR
                // outside a bracketed paste (proposal 0024); cosmetic input
                // (focus events, cursor) never arms. This is the single input
                // choke point, so it covers web, ccs, and hub-relayed clients.
                if scan_for_submit(data, &mut st.in_bracketed_paste) {
                    st.busy_since = now;
                    st.busy_until = now + WORK_GRACE_SECS;
                }
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
        let Ok(mut w) = self.writer.lock() else {
            return (0, Some(std::io::Error::other("writer poisoned")));
        };
        let mut written = 0usize;
        while written < data.len() {
            match w.write(&data[written..]) {
                Ok(0) => {
                    return (written, Some(std::io::ErrorKind::WriteZero.into()));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return (written, Some(e)),
            }
        }
        match w.flush() {
            Ok(()) => (written, None),
            Err(e) => (written, Some(e)),
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

    /// The operator-chosen mark colour (proposal 0029), or `None` when unmarked.
    pub fn color(&self) -> Option<String> {
        self.color.lock().ok().and_then(|c| c.clone())
    }

    /// Set (or clear, with `None`) the live mark colour. Persistence to the
    /// manifest is the caller's job (see `handlers::set_color_core`).
    pub fn set_color(&self, color: Option<String>) {
        if let Ok(mut c) = self.color.lock() {
            *c = color;
        }
    }

    /// The operator-chosen display label (proposal 0035), or `None` when unset.
    pub fn label(&self) -> Option<String> {
        self.label.lock().ok().and_then(|l| l.clone())
    }

    /// Set (or clear, with `None`) the live display label. Persistence to the
    /// manifest is the caller's job (see `handlers::set_label_core`).
    pub fn set_label(&self, label: Option<String>) {
        if let Ok(mut l) = self.label.lock() {
            *l = label;
        }
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

    /// The busy-window deadline. While working it's in the future; once ready it
    /// equals the busy→ready transition instant and is not bumped by cosmetic
    /// output — clients anchor the "ready for N" timer/sort to it (proposal 0024).
    pub fn busy_until(&self) -> u64 {
        self.state.lock().map(|s| s.busy_until).unwrap_or(0)
    }

    /// True when the session is **not** in an open, submit-armed busy window — the
    /// "your turn" / ready signal the clients surface. Under the input-gated model
    /// (proposal 0024) this is `now >= busy_until`: a session reads ready until a
    /// user submit opens a window, and again once the window lapses after the
    /// agent's output goes quiet. A session never submitted to reads ready.
    pub fn waiting(&self) -> bool {
        self.waiting_at(now_secs())
    }

    pub fn waiting_at(&self, now: u64) -> bool {
        let busy_until = self.state.lock().map(|s| s.busy_until).unwrap_or(0);
        !is_working(busy_until, now)
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
                    // Sustain an *active* turn: output that lands while the busy
                    // window is still open pushes the deadline out. Output after
                    // the window has lapsed (or with no turn ever armed) is
                    // cosmetic — a focus/resize repaint, the cursor, the spinner —
                    // and must NOT reopen it; only a user submit does (see
                    // `write_input`). This is what stops focusing a session from
                    // flashing it busy. Proposal 0024.
                    if is_working(st.busy_until, now) {
                        st.busy_until = now + WORK_GRACE_SECS;
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
    /// Sessions currently being restarted by the assistant-update job (proposal
    /// 0049). A stop is normally a goodbye — the reaper forgets a cleanly-exited
    /// session's manifest entry on purpose — so a restart needs to say so
    /// explicitly rather than rely on SIGKILL's side effects. The reaper
    /// *consumes* the marker (see `create`), which is what makes the graceful
    /// `/exit` stop safe: the CLI flushes the transcript `--continue` will read,
    /// and the entry survives.
    pub restarting: Mutex<std::collections::HashSet<String>>,
    /// The current-or-last assistant-update job (proposal 0049). One at a time
    /// per agent; retained after `done` so a client that reloads (or a phone that
    /// was asleep) still sees the result — which is why this lives here and not
    /// in React state.
    pub update_job: Mutex<cc_screen_protocol::UpdateJob>,
    /// Set when an install changed which CLIs exist here (proposal 0050 C4), so
    /// the uplink re-advertises the tool registry on its next tick. A flag rather
    /// than a poll: the hub caches `unavailable` from `Register` and a direct
    /// client live-probes per request, so this is the *only* consumer — and
    /// re-probing the filesystem every second to notice a once-a-month event
    /// would be the wrong trade.
    pub tools_dirty: std::sync::atomic::AtomicBool,
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
    /// Durable per-session image attachments for path-paste assistants
    /// (proposal 0066; Codex). Opened against the config dir at startup so GC
    /// + quota reconstruction happen before any paste is accepted.
    pub attachments: crate::clip_attachment::AttachmentStore,
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
        // Startup GC + quota reconstruction for durable image attachments
        // (0066): a directory survives iff a manifest record claims it (the
        // registry is empty this early, so the manifest IS the claim set).
        let claimed: Vec<String> =
            manifest::entries(&config_dir).into_iter().map(|e| e.session).collect();
        let attachments = crate::clip_attachment::AttachmentStore::open(&config_dir, &claimed);
        AppState {
            inner: Arc::new(Inner {
                tools,
                registry: Mutex::new(HashMap::new()),
                restarting: Mutex::new(std::collections::HashSet::new()),
                update_job: Mutex::new(cc_screen_protocol::UpdateJob::idle()),
                tools_dirty: std::sync::atomic::AtomicBool::new(false),
                env_path,
                clip_url,
                push: crate::push::Push::new(&config_dir),
                config_dir,
                watcher: crate::watch::Watcher::new(home.clone()),
                home,
                machine_id,
                clip: ClipStore::default(),
                attachments,
                auth,
                origin,
                login_throttle: cc_screen_auth::LoginThrottle::new(),
            }),
        }
    }

    pub fn find_tool(&self, key: &str) -> Option<Tool> {
        self.inner.tools.iter().find(|t| t.cmd == key || t.prefix == key).cloned()
    }

    /// `Some(binary)` iff `tool` needs a CLI that isn't on the session PATH
    /// (proposal 0046). Probes the exact `env_path` sessions spawn with, so the
    /// verdict matches what `/bin/sh -c` would find. `None` = safe to launch
    /// (present, or nothing to probe — the shell tool).
    pub fn tool_binary_missing(&self, tool: &Tool) -> Option<String> {
        tools::missing_binary(tool, &self.inner.env_path)
    }

    /// The tool list as wire DTOs with the live availability probe (0046).
    /// Shared by `GET /api/tools` and the hub uplink's `Register`, so a direct
    /// client and a hub-relayed one see the same `unavailable` flags.
    pub fn tool_infos(&self) -> Vec<cc_screen_protocol::ToolInfo> {
        self.inner
            .tools
            .iter()
            .map(|t| tools::tool_info(t, self.tool_binary_missing(t).is_some()))
            .collect()
    }

    /// Availability changed — ask the uplink to re-advertise the tool registry
    /// (proposal 0050 C4). No-op for a standalone agent, whose `/api/tools`
    /// live-probes per request anyway.
    pub fn announce_tools(&self) {
        self.inner.tools_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Consume the flag above — `true` exactly once per `announce_tools`.
    pub fn take_tools_dirty(&self) -> bool {
        self.inner.tools_dirty.swap(false, std::sync::atomic::Ordering::Relaxed)
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
                // A fresh session starts unmarked; a restored one re-applies its
                // saved colour below (this record overwrote any prior entry).
                color: String::new(),
                // Likewise unlabelled on create; restore re-applies it below.
                label: String::new(),
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
            //
            // …unless this stop is a RESTART (proposal 0049): the assistant-update
            // job marked the session before typing `/exit`, and a restart is not a
            // goodbye. Consuming the marker here (rather than having the restarter
            // clear it) closes the race — the forget decision and the un-marking
            // are the same atomic step, so the relaunched session's own later exit
            // behaves normally.
            let restarting = inner.restarting.lock().unwrap().remove(&key);
            if matches!(status, Ok(s) if s.success()) && !restarting {
                manifest::forget(&inner.config_dir, &key);
                // Permanent goodbye → its durable image attachments go too
                // (0066). A restart/resume stop keeps them: a Codex draft or
                // transcript may still reference the staged paths.
                inner.attachments.purge_session(&key);
            }
        });
        Ok(full)
    }

    /// Bring back every recorded-but-not-live session, resuming its conversation.
    /// Idempotent; used by POST /api/sessions/restore and at startup.
    pub fn restore_all(&self) -> (Vec<String>, HashMap<String, String>) {
        let (restored, failed) = self.restore_matching(&[]);
        (restored.into_iter().map(|(name, _)| name).collect(), failed.into_iter().collect())
    }

    /// Restore only the recorded sessions belonging to `prefixes` (proposal
    /// 0050 C3): after an install job makes a CLI appear, the manifest entries
    /// Restore has been skipping on every startup for that CLI come back — under
    /// their original names, with their marks and labels — instead of waiting for
    /// the next agent restart. Shares `restore_all`'s body, including the
    /// colour/label re-apply, so there is one restore recipe.
    ///
    /// Returns `(restored (session, tool), failed (session, why))`.
    pub fn restore_prefixes(&self, prefixes: &[String]) -> (Vec<(String, String)>, Vec<(String, String)>) {
        self.restore_matching(prefixes)
    }

    /// The shared body. Empty `prefixes` = every recorded session.
    fn restore_matching(&self, prefixes: &[String]) -> (Vec<(String, String)>, Vec<(String, String)>) {
        let live: std::collections::HashSet<String> =
            self.inner.registry.lock().unwrap().keys().cloned().collect();
        let mut restored: Vec<(String, String)> = Vec::new();
        let mut failed: Vec<(String, String)> = Vec::new();
        for e in manifest::entries(&self.inner.config_dir) {
            if live.contains(&e.session) {
                continue;
            }
            let Some(tool) = self.find_tool(&e.prefix).or_else(|| self.find_tool(&e.cmd)) else {
                continue;
            };
            if !prefixes.is_empty() && !prefixes.iter().any(|p| p == &tool.prefix) {
                continue;
            }
            if !std::path::Path::new(&e.dir).is_dir() {
                continue;
            }
            // A recorded session whose CLI has since gone missing would spawn a
            // shell that exits 127 — and a non-clean exit keeps the manifest
            // entry, so it would retry on every startup (0046). Skip it, loudly:
            // it lands in `failed` (surfaced by the restore response + the
            // startup log) and stays in the manifest, so installing the CLI and
            // restoring brings it back.
            if let Some(bin) = self.tool_binary_missing(&tool) {
                let hint = tools::install_hint(&tool)
                    .map(|c| format!(" — install it with: {c}"))
                    .unwrap_or_default();
                tracing::warn!("restore: skipping {} — `{bin}` is not installed{hint}", e.session);
                failed.push((e.session.clone(), format!("{bin} is not installed{hint}")));
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
                Ok(name) => {
                    // `create` re-recorded the manifest entry with an empty colour
                    // — re-apply the saved mark to both the live session and the
                    // persisted entry so it survives the restore (proposal 0029).
                    if !e.color.is_empty() {
                        if let Some(sess) = self.get(&name) {
                            sess.set_color(Some(e.color.clone()));
                        }
                        manifest::set_color(&self.inner.config_dir, &name, Some(e.color.clone()));
                    }
                    // Same re-apply for the saved display label (proposal 0035).
                    if !e.label.is_empty() {
                        if let Some(sess) = self.get(&name) {
                            sess.set_label(Some(e.label.clone()));
                        }
                        manifest::set_label(&self.inner.config_dir, &name, Some(e.label.clone()));
                    }
                    restored.push((name, tool.prefix.clone()));
                }
                Err(err) => {
                    failed.push((e.session.clone(), err.to_string()));
                }
            }
        }
        (restored, failed)
    }

    // ── Assistant-update job: restarting sessions (proposal 0049) ─────────────
    /// The live sessions an update of `prefixes` would restart: `(name, tool)`,
    /// sorted. Used to seed the job's session rows as `pending` before phase 2
    /// starts, so the UI shows what's coming rather than growing a list. The
    /// bare `shell` tool is excluded structurally — it isn't an assistant, has
    /// nothing to update and no resume, so restarting it would silently discard
    /// the user's shell state.
    pub fn restart_targets(&self, prefixes: &[String]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .list()
            .into_iter()
            .filter(|s| prefixes.iter().any(|p| p == &s.tool))
            .map(|s| (s.name.clone(), s.tool.clone()))
            .collect();
        v.sort();
        v
    }

    /// Stop each live session whose tool is in `prefixes` **without letting the
    /// manifest forget it**, then relaunch it with `resume = true` and re-apply
    /// its colour + label — the same recipe `restore_all` runs per entry, driven
    /// from the live registry instead of the manifest.
    ///
    /// `progress` is called on every state transition (`stopping` → `starting` →
    /// terminal) so a watching client sees per-session movement, not a spinner.
    ///
    /// Nothing here removes a manifest entry: a session that won't stop is left
    /// running and reported; one that won't relaunch keeps its entry so the
    /// standing Restore can retry it.
    pub fn restart_sessions(
        &self,
        prefixes: &[String],
        mut progress: impl FnMut(&SessionRestartStatus),
    ) -> Vec<SessionRestartStatus> {
        let mut out = Vec::new();
        for (name, tool_prefix) in self.restart_targets(prefixes) {
            let mut row = SessionRestartStatus {
                session: name.clone(),
                tool: tool_prefix.clone(),
                state: "stopping".into(),
                message: None,
            };
            progress(&row);

            let Some(sess) = self.get(&name) else {
                // It ended between the seed and now — nothing to restart.
                row.state = "skipped".into();
                row.message = Some("session already gone".into());
                progress(&row);
                out.push(row);
                continue;
            };
            // Snapshot the launch spec BEFORE stopping, so the relaunch never
            // depends on the manifest surviving the stop. The manifest entry is
            // authoritative (it carries colour/label/extra dirs); the live
            // session is the fallback for a session recorded before those.
            let entry = manifest::entries(&self.inner.config_dir)
                .into_iter()
                .find(|e| e.session == name);
            let (short, dir, extra_dirs, skip_permissions, color, label) = match &entry {
                Some(e) => (
                    e.short.clone(),
                    e.dir.clone(),
                    e.extra_dirs.clone(),
                    e.skip_permissions,
                    e.color.clone(),
                    e.label.clone(),
                ),
                None => (
                    sess.short.clone(),
                    sess.launch_dir.clone(),
                    sess.extra_dirs.clone(),
                    sess.skip_permissions,
                    sess.color().unwrap_or_default(),
                    sess.label().unwrap_or_default(),
                ),
            };
            let Some(tool) = self
                .find_tool(&tool_prefix)
                .or_else(|| entry.as_ref().and_then(|e| self.find_tool(&e.cmd)))
            else {
                row.state = "skipped".into();
                row.message = Some(format!("no tool registered for {tool_prefix}"));
                progress(&row);
                out.push(row);
                continue;
            };

            // Mark first, then stop gracefully: `/exit` lets the CLI flush its own
            // transcript, which is exactly what `--continue` reads back.
            self.inner.restarting.lock().unwrap().insert(name.clone());
            sess.graceful_exit();
            if !self.wait_gone(&name, GRACEFUL_STOP) {
                sess.kill();
                if !self.wait_gone(&name, FORCED_STOP) {
                    // Leave it running and untouched — a stuck session must not
                    // abort the rest of the job.
                    self.inner.restarting.lock().unwrap().remove(&name);
                    row.state = "failed".into();
                    row.message = Some("session did not stop; left running".into());
                    progress(&row);
                    out.push(row);
                    continue;
                }
            }

            row.state = "starting".into();
            row.message = None;
            progress(&row);
            match self.create(&tool, &short, &dir, extra_dirs, true, skip_permissions) {
                Ok(new_name) => {
                    // `create` re-records the entry blank — re-apply the saved
                    // mark + label exactly as `restore_all` does.
                    if !color.is_empty() {
                        if let Some(s) = self.get(&new_name) {
                            s.set_color(Some(color.clone()));
                        }
                        manifest::set_color(&self.inner.config_dir, &new_name, Some(color.clone()));
                    }
                    if !label.is_empty() {
                        if let Some(s) = self.get(&new_name) {
                            s.set_label(Some(label.clone()));
                        }
                        manifest::set_label(&self.inner.config_dir, &new_name, Some(label.clone()));
                    }
                    row.state = "resumed".into();
                }
                Err(e) => {
                    // Keep it restorable rather than lost: re-record what we
                    // snapshotted so the standing Restore path can retry it.
                    if let Some(e0) = entry.clone() {
                        manifest::record(&self.inner.config_dir, e0);
                    }
                    row.state = "failed".into();
                    row.message = Some(e.to_string());
                }
            }
            progress(&row);
            out.push(row);
        }
        out
    }

    /// Block until `name` leaves the live registry (the reaper drops it the
    /// instant the child exits), or `budget` elapses. Returns whether it went.
    fn wait_gone(&self, name: &str, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        loop {
            if self.get(name).is_none() {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The current-or-last update job snapshot (proposal 0049).
    pub fn update_job(&self) -> cc_screen_protocol::UpdateJob {
        self.inner.update_job.lock().unwrap().clone()
    }

    /// Mutate the live job under its lock — the single write path the worker uses
    /// for every row transition, so a concurrent `GET` always reads a consistent
    /// snapshot.
    pub fn with_update_job(&self, f: impl FnOnce(&mut cc_screen_protocol::UpdateJob)) {
        let mut j = self.inner.update_job.lock().unwrap();
        f(&mut j);
    }

    /// Install a fresh job iff none is running. `Err` carries the running job's
    /// snapshot, which the caller turns into a `409` — the client then just
    /// switches to watching it instead of starting a second one.
    pub fn begin_update_job(
        &self,
        job: cc_screen_protocol::UpdateJob,
    ) -> Result<cc_screen_protocol::UpdateJob, cc_screen_protocol::UpdateJob> {
        let mut cur = self.inner.update_job.lock().unwrap();
        if cur.running() {
            return Err(cur.clone());
        }
        *cur = job;
        Ok(cur.clone())
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
            install_hint: None,
            update_cmd: None,
            image_paste: crate::tools::ImagePasteStrategy::ClipboardProbe,
        }
    }


    // Proposal 0050 C3. `restore_all`'s own comment says a session skipped for a
    // missing CLI comes back once the CLI is installed — nothing performed that
    // sentence's second half at the moment the CLI appeared. `restore_prefixes`
    // does, and it must (a) touch ONLY the installed tool's entries and (b) reuse
    // `restore_all`'s recipe, including the colour/label re-apply.
    #[tokio::test]
    async fn restore_prefixes_touches_only_that_tool_and_keeps_mark_and_label() {
        let tmp = std::env::temp_dir().join(format!("ccr-restp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut alpha = shell_tool("sleep 30");
        alpha.cmd = "al".into();
        alpha.prefix = "alpha".into();
        let mut beta = shell_tool("sleep 30");
        beta.cmd = "be".into();
        beta.prefix = "beta".into();
        let state = AppState::new(
            vec![alpha.clone(), beta.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let dir = tmp.to_string_lossy().to_string();
        let a = state.create(&alpha, "work", &dir, vec![], false, true).unwrap();
        let b = state.create(&beta, "work", &dir, vec![], false, true).unwrap();
        // A marked + labelled session, so the re-apply is actually observable.
        state.get(&a).unwrap().set_color(Some("teal".into()));
        manifest::set_color(&tmp, &a, Some("teal".into()));
        state.get(&a).unwrap().set_label(Some("Auth refactor".into()));
        manifest::set_label(&tmp, &a, Some("Auth refactor".into()));

        // Kill both: a hard exit is NOT a goodbye, so both manifest entries stay
        // — the state a machine is in after the CLI went missing.
        state.get(&a).unwrap().kill();
        state.get(&b).unwrap().kill();
        for _ in 0..100 {
            if state.get(&a).is_none() && state.get(&b).is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(state.get(&a).is_none() && state.get(&b).is_none(), "reaper cleared the registry");

        // Only alpha's CLI "appeared", so only alpha's sessions come back.
        let (restored, failed) = state.restore_prefixes(&["alpha".to_string()]);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(restored, vec![(a.clone(), "alpha".to_string())]);
        assert!(state.get(&a).is_some(), "alpha is live again under its original name");
        assert!(state.get(&b).is_none(), "beta was not touched");

        // …with its mark and label intact, live and persisted.
        assert_eq!(state.get(&a).unwrap().color().as_deref(), Some("teal"));
        assert_eq!(state.get(&a).unwrap().label().as_deref(), Some("Auth refactor"));
        let entry = manifest::entries(&tmp).into_iter().find(|e| e.session == a).unwrap();
        assert_eq!(entry.color, "teal");
        assert_eq!(entry.label, "Auth refactor");

        // An empty prefix list is "everything" — the shape `restore_all` uses.
        let (all, _) = state.restore_prefixes(&[]);
        assert_eq!(all, vec![(b.clone(), "beta".to_string())], "alpha is already live");

        state.get(&a).unwrap().kill();
        state.get(&b).unwrap().kill();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn working_while_busy_window_open() {
        // Working iff the submit-armed deadline is still in the future (0024).
        let now = 1_000_000;
        assert!(is_working(now + WORK_GRACE_SECS, now), "deadline ahead → working");
        assert!(is_working(now + 1, now), "one second left → working");
        assert!(!is_working(now, now), "deadline reached → waiting");
        assert!(!is_working(now - 3600, now), "deadline long past → waiting");
        assert!(!is_working(0, now), "never armed (busy_until = 0) → waiting");
    }

    #[test]
    fn submit_detection_arms_only_on_real_enter() {
        let mut p = false;
        assert!(scan_for_submit(b"\r", &mut p), "a bare CR is a submit");
        assert!(scan_for_submit(b"y\r", &mut p), "text then CR is a submit");
        assert!(!scan_for_submit(b"y", &mut p), "no CR → no submit");
        assert!(!scan_for_submit(b"hello world", &mut p), "plain text → no submit");
        assert!(!scan_for_submit(b"\x1b\r", &mut p), "ESC-prefixed CR (newline escape) → not a submit");
        assert!(!scan_for_submit(b"\x1b[I", &mut p), "a focus-in event is not a submit");

        // Bracketed paste: a CR *inside* the markers is pasted text (not a submit);
        // a CR *after* the closing marker (wrap_bracketed_paste enter=true) is.
        let mut p2 = false;
        assert!(!scan_for_submit(b"\x1b[200~a\rb\x1b[201~", &mut p2), "CR inside paste → not a submit");
        assert!(!p2, "paste closed in the same chunk → flag reset");
        assert!(scan_for_submit(b"\x1b[200~pasted\x1b[201~\r", &mut p2), "CR after paste close → submit");

        // A paste spanning two write_input calls: the flag persists, inner CRs ignored.
        let mut p3 = false;
        assert!(!scan_for_submit(b"\x1b[200~line1\r", &mut p3), "open paste, inner CR ignored");
        assert!(p3, "still inside the paste across calls");
        assert!(!scan_for_submit(b"line2\rline3\x1b[201~", &mut p3), "still in paste → inner CRs ignored");
        assert!(!p3, "paste now closed across calls");
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

    // End-to-end over a real PTY + real time for the input-gated model (0024):
    // a session reads ready until a *submit* arms it, stays working while the
    // window is open, and flips back to ready a grace-window after output stops.
    // The session's own startup output must NOT arm it (only a submit does).
    #[tokio::test]
    async fn busy_is_submit_armed_then_grace_released() {
        let tmp = std::env::temp_dir().join(format!("ccr-busy-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // A quiet shell that stays alive — no auto output to lean on; we drive the
        // state purely via a submit so the test asserts the arming semantics.
        let tool = shell_tool("sleep 30");
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

        // Before any submit (and despite any startup repaint), it reads ready.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(sess.waiting(), "a session never submitted to reads as ready");

        // A submit arms the busy window immediately, with no agent output needed.
        sess.write_input(b"\r");
        assert!(!sess.waiting(), "right after Enter it reads as working");
        assert!(sess.busy_since() > 0, "busy_since is stamped at the submit (turn start)");

        // Still inside the window a few seconds later → still working.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(!sess.waiting(), "within the grace window it stays working");

        // Past the window with no further output → back to ready.
        tokio::time::sleep(std::time::Duration::from_secs(WORK_GRACE_SECS + 2)).await;
        assert!(sess.waiting(), "a grace window after output stops it reads ready again");

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

    // Proposal 0077 Part E. A copy performed INSIDE a session travels to the
    // client in-band, as the OSC 52 bytes the assistant already emits — there is
    // no new route and no new wire message, which is only true for as long as
    // the agent keeps relaying terminal output verbatim. This pins that: the
    // sequence must reach a live subscriber byte-for-byte, with no filtering, no
    // OSC allowlist, and no rewriting.
    //
    // It also documents the known limit the proposal accepts: `snapshot()`
    // re-serializes GRID STATE, so a sequence emitted before a client attached
    // (or during a Lagged resync) is unrecoverable by construction. Only
    // sequences emitted while a client is attached are deliverable.
    #[tokio::test]
    async fn osc52_survives_the_broadcast_path_but_not_the_snapshot() {
        let tmp = std::env::temp_dir().join(format!("ccr-osc52-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // aGkgdGhlcmU= is "hi there". Emitted after a beat so a client is
        // attached before it lands — the deliverable case.
        let tool = shell_tool("printf BEFORE_MARK; sleep 1; printf '\\033]52;c;aGkgdGhlcmU=\\007'; sleep 3");
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

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let (snap, mut rx) = sess.attach();

        // Collect the live stream for long enough to see the sequence.
        let mut live: Vec<u8> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await {
                Ok(Ok(chunk)) => {
                    live.extend_from_slice(&chunk);
                    if live.windows(4).any(|w| w == b"]52;") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }
        let live_s = String::from_utf8_lossy(&live).into_owned();
        assert!(
            live_s.contains("\u{1b}]52;c;aGkgdGhlcmU=\u{7}"),
            "OSC 52 must reach an attached client verbatim; saw {live_s:?}"
        );

        // The documented limit: the snapshot is grid state, so a pre-attach
        // sequence is gone. (BEFORE_MARK, printed at the same time, IS there —
        // it painted a cell.)
        let snap_s = String::from_utf8_lossy(&snap);
        assert!(snap_s.contains("BEFORE_MARK"));
        assert!(
            !snap_s.contains("]52;"),
            "a snapshot re-serializes grid state; it cannot carry an escape sequence"
        );

        sess.kill();
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

    /// A tool whose session ends cleanly (status 0) the moment `/exit` is typed:
    /// `read` consumes the line and the shell exits. That's the exact shape the
    /// restart path relies on — a graceful stop that the reaper would normally
    /// treat as a deliberate goodbye.
    fn exits_on_input(prefix: &str) -> Tool {
        let mut t = shell_tool("read x");
        t.prefix = prefix.into();
        t
    }

    fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        false
    }

    // The load-bearing rule of proposal 0049: a CLEAN exit is normally a
    // deliberate goodbye and the reaper forgets the manifest entry — but not when
    // the session is marked as restarting. Without this, the only non-destructive
    // stop is SIGKILL and "restart" would depend on a side effect of the
    // exit-status rule rather than on stated intent.
    #[test]
    fn a_clean_exit_while_restarting_keeps_the_manifest_entry() {
        let tmp = std::env::temp_dir().join(format!("ccr-restartmark-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tool = exits_on_input("shell");
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

        // Control: an unmarked clean exit still forgets (today's behavior).
        let plain = state.create(&tool, "plain", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        state.get(&plain).unwrap().graceful_exit();
        assert!(wait_until(|| state.get(&plain).is_none()), "the child should exit on /exit");
        assert!(
            wait_until(|| !manifest::entries(&tmp).iter().any(|e| e.session == plain)),
            "an unmarked clean exit is a goodbye → forgotten"
        );

        // Marked as restarting: the same clean exit must NOT forget it.
        let kept = state.create(&tool, "kept", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        state.inner.restarting.lock().unwrap().insert(kept.clone());
        state.get(&kept).unwrap().graceful_exit();
        assert!(wait_until(|| state.get(&kept).is_none()));
        std::thread::sleep(std::time::Duration::from_millis(300)); // let the reaper finish
        assert!(
            manifest::entries(&tmp).iter().any(|e| e.session == kept),
            "a restart is not a goodbye — the entry must survive"
        );
        // …and the marker is consumed, so the relaunched session's own later exit
        // behaves normally again.
        assert!(!state.inner.restarting.lock().unwrap().contains(&kept));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // End-to-end restart (0049 Part D): the session comes back under the SAME
    // name — so an attached pane re-attaches with no user action — with its mark,
    // label and manifest entry intact, and the job reports it `resumed`.
    #[test]
    fn restart_sessions_brings_them_back_named_and_marked() {
        let tmp = std::env::temp_dir().join(format!("ccr-restart-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let tool = exits_on_input("claude");
        let other = {
            let mut t = exits_on_input("codex");
            t.cmd = "coc".into();
            t
        };
        let state = AppState::new(
            vec![tool.clone(), other.clone()],
            std::env::var("PATH").unwrap_or_default(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        let name = state.create(&tool, "proj", &tmp.to_string_lossy(), vec![], false, false).unwrap();
        let untouched = state.create(&other, "infra", &tmp.to_string_lossy(), vec![], false, true).unwrap();
        let before_pid = state.get(&name).unwrap().pid;
        crate::handlers::set_color_core(&state, &name, Some("teal".into())).unwrap();
        crate::handlers::set_label_core(&state, &name, Some("My project".into())).unwrap();

        let mut seen: Vec<String> = Vec::new();
        let rows = state.restart_sessions(&["claude".to_string()], |r| seen.push(r.state.clone()));

        assert_eq!(rows.len(), 1, "only the named tool's sessions restart: {rows:?}");
        assert_eq!(rows[0].session, name);
        assert_eq!(rows[0].state, "resumed", "{:?}", rows[0]);
        assert_eq!(seen, vec!["stopping", "starting", "resumed"], "per-session progress is reported");

        // Same name, new process, marks preserved on both the live session and
        // the manifest (so a later restore keeps them too).
        let back = state.get(&name).expect("the session is live again under the same name");
        assert_ne!(back.pid, before_pid, "it really is a new process");
        assert_eq!(back.color().as_deref(), Some("teal"));
        assert_eq!(back.label().as_deref(), Some("My project"));
        assert_eq!(back.skip_permissions, false, "the launch policy is preserved");
        let entry = manifest::entries(&tmp).into_iter().find(|e| e.session == name).expect("entry kept");
        assert_eq!(entry.color, "teal");
        assert_eq!(entry.label, "My project");

        // The other tool's session was never touched.
        assert!(state.get(&untouched).is_some(), "a codex session isn't churned because claude moved");

        for s in state.list() {
            s.kill();
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // Restore hygiene (0046): a recorded session whose CLI has since gone missing
    // is skipped — reported in `failed`, entry kept — instead of spawning a shell
    // that exits 127 and (per the reaper's non-clean-exit rule) retries forever.
    #[test]
    fn restore_skips_a_missing_tool_without_looping() {
        let tmp = std::env::temp_dir().join(format!("ccr-restore-miss-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ghost = Tool {
            cmd: "gh".into(),
            prefix: "ghost".into(),
            tmpl: "ghost-cli".into(),
            extra_flag: None,
            extra_max: 0,
            resume_suffix: None,
            resume_keep_extra: false,
            yolo_flag: None,
            install_hint: None,
            update_cmd: None,
            image_paste: crate::tools::ImagePasteStrategy::ClipboardProbe,
        };
        // env_path = the empty tmp dir → `ghost-cli` can't resolve.
        let state = AppState::new(
            vec![ghost],
            tmp.to_string_lossy().into_owned(),
            String::new(),
            tmp.clone(),
            tmp.clone(),
            "test-agent".into(),
            crate::auth::Auth::load(&tmp, None, None),
            cc_screen_auth::OriginPolicy::default(),
        );
        manifest::record(
            &tmp,
            manifest::Entry {
                session: "ghost-old".into(),
                cmd: "gh".into(),
                prefix: "ghost".into(),
                short: "old".into(),
                dir: tmp.to_string_lossy().into_owned(),
                extra_dirs: vec![],
                created_at: 0,
                skip_permissions: true,
                color: String::new(),
                label: String::new(),
            },
        );
        let (restored, failed) = state.restore_all();
        assert!(restored.is_empty(), "must not spawn a doomed session");
        assert!(state.list().is_empty(), "nothing may be registered");
        let msg = failed.get("ghost-old").expect("the skip must be reported");
        assert!(msg.contains("ghost-cli is not installed"), "{msg}");
        // The entry survives, so installing the CLI + restoring brings it back.
        assert_eq!(manifest::entries(&tmp).len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
