//! End-to-end tests for the `ccs` TUI (proposal 0059 B3).
//!
//! Each test starts an in-process hub + one or more fake agents (the SAME
//! wire-contract doubles the hub's own suite uses, hoisted into
//! `cc_screen_hub::test_support` by B1), builds a real `App` pointed at the hub's
//! ephemeral port, drives it with synthetic `AppMsg`s, renders into a
//! `TestBackend`, and asserts on the buffer text (and on what the fake agent
//! recorded). No real terminal, no sleeps for correctness (we wait on actual
//! channel messages / on real hub state with a generous timeout), no docker.

use std::sync::Once;
use std::time::Duration;

use cc_screen_auth::Auth;
use cc_screen_hub::test_support::{sess, spawn_scriptable_agent, start_hub, FakeAgentHandle};
use cc_screen_protocol::hub::Cmd;
use cc_screen_protocol::SessionInfo;
use cc_screen_tui::app::{App, AppMsg};
use cc_screen_tui::client::Rest;
use cc_screen_tui::config::Config;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc;

/// Redirect the TUI's config writes (recents persistence, `Config::save`) to a
/// throwaway dir so running the suite never clobbers a developer's real
/// `~/.config/cc-screen-tui/config.toml`. `directories` reads `XDG_CONFIG_HOME`
/// on Linux (where CI runs); idempotent via `Once`.
fn isolate_config() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ccs-e2e-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    });
}

/// A booted app + its channel receiver + a `TestBackend` terminal, ready to drive.
struct Harness {
    app: App,
    rx: mpsc::Receiver<AppMsg>,
    term: Terminal<TestBackend>,
}

impl Harness {
    /// Build an `App` against `http://{hub}`, run its async `init`, draw once.
    async fn boot(hub: &str, cols: u16, rows: u16) -> Harness {
        Self::boot_auth(hub, cols, rows, None).await
    }

    async fn boot_auth(hub: &str, cols: u16, rows: u16, token: Option<String>) -> Harness {
        isolate_config();
        let rest = Rest::new(&format!("http://{hub}"), false, token).expect("rest");
        let mut app = App::new(rest, Config::default());
        let rx = app.take_rx();
        app.init().await;
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        app.draw(&mut term).unwrap();
        Harness { app, rx, term }
    }

    /// Boot with a `ccs <session>` direct-attach query armed (0059 C2). The raw
    /// query is handed to the app, which resolves it via `App::resolve_attach`
    /// against the freshly-refreshed session list during `init()` — booting
    /// straight into the attached grid instead of the action menu.
    async fn boot_attached(hub: &str, cols: u16, rows: u16, query: &str) -> Harness {
        isolate_config();
        let rest = Rest::new(&format!("http://{hub}"), false, None).expect("rest");
        let mut app = App::new(rest, Config::default());
        app.set_start_attach(query.into());
        let rx = app.take_rx();
        app.init().await;
        let mut term = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        app.draw(&mut term).unwrap();
        Harness { app, rx, term }
    }

    /// Send a synthetic key.
    async fn key(&mut self, code: KeyCode) {
        self.send(AppMsg::Term(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))).await;
    }

    /// Send a `Ctrl-<c>` key (the grid prefix is Ctrl-A).
    async fn ctrl(&mut self, c: char) {
        self.send(AppMsg::Term(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL,
        ))))
        .await;
    }

    /// Type a string of printable chars, one key at a time.
    async fn type_str(&mut self, s: &str) {
        for c in s.chars() {
            self.key(KeyCode::Char(c)).await;
        }
    }

    /// Deliver a `Tick` (the poll refresh the real ticker would fire).
    async fn tick(&mut self) {
        self.send(AppMsg::Tick).await;
    }

    /// Resize both the backing terminal and the app (what a real SIGWINCH does).
    async fn resize(&mut self, cols: u16, rows: u16) {
        self.term.backend_mut().resize(cols, rows);
        self.send(AppMsg::Term(Event::Resize(cols, rows))).await;
    }

    async fn send(&mut self, msg: AppMsg) {
        let tx = self.app.tx();
        tx.send(msg).await.expect("send");
        // Immediately drain + redraw so a caller can assert right after.
        self.pump_once().await;
    }

    /// Drain whatever is queued (without blocking), handle it, redraw one frame.
    async fn pump_once(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.app.handle_msg(m).await;
        }
        self.app.draw(&mut self.term).unwrap();
    }

    /// Pump the loop until `pred(buffer_text)` holds or we exhaust the budget.
    /// Waits on real channel messages (so async network work — attach snapshots,
    /// create replies, session polls — can arrive) with a per-step timeout.
    async fn pump_until<F: Fn(&str) -> bool>(&mut self, pred: F) -> bool {
        if pred(&self.text()) {
            return true;
        }
        for _ in 0..200 {
            match tokio::time::timeout(Duration::from_millis(200), self.rx.recv()).await {
                Ok(Some(m)) => {
                    self.app.handle_msg(m).await;
                    while let Ok(m) = self.rx.try_recv() {
                        self.app.handle_msg(m).await;
                    }
                    self.app.draw(&mut self.term).unwrap();
                    if pred(&self.text()) {
                        return true;
                    }
                }
                // No message this step: still redraw + re-check (a poll refresh may
                // have mutated state via an earlier message) and keep waiting.
                _ => {
                    self.app.draw(&mut self.term).unwrap();
                    if pred(&self.text()) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Like `pump_until`, but the predicate inspects *external* state (e.g. what
    /// the fake agent recorded through the hub) instead of the buffer. Pumps the
    /// app so the round-trip (input → hub → agent, create reply, …) can complete.
    async fn pump_cond<F: FnMut() -> bool>(&mut self, mut cond: F) -> bool {
        if cond() {
            return true;
        }
        for _ in 0..200 {
            match tokio::time::timeout(Duration::from_millis(200), self.rx.recv()).await {
                Ok(Some(m)) => {
                    self.app.handle_msg(m).await;
                    while let Ok(m) = self.rx.try_recv() {
                        self.app.handle_msg(m).await;
                    }
                    self.app.draw(&mut self.term).unwrap();
                }
                _ => {
                    self.app.draw(&mut self.term).unwrap();
                }
            }
            if cond() {
                return true;
            }
        }
        false
    }

    /// Repeatedly deliver a `Tick` (re-polling `/api/sessions`) until `pred` holds
    /// on the rendered buffer. A short yield between ticks lets in-process agent
    /// pushes reach the hub; the loop's exit is the real condition, not the sleep.
    async fn tick_until<F: Fn(&str) -> bool>(&mut self, pred: F) -> bool {
        for _ in 0..60 {
            if pred(&self.text()) {
                return true;
            }
            self.tick().await;
            if pred(&self.text()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pred(&self.text())
    }

    /// Flatten the `TestBackend` buffer into a single string (row by row).
    fn text(&self) -> String {
        let buf = self.term.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

// ── free helpers ────────────────────────────────────────────────────────────

/// GET the hub's session list (with an optional bearer token).
async fn hub_sessions(hub: &str, token: Option<&str>) -> Vec<SessionInfo> {
    let mut req = reqwest::Client::new().get(format!("http://{hub}/api/sessions"));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    match req.send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Poll the hub until its advertised session list satisfies `pred` — the
/// deterministic way to know an agent's `Sessions` push has propagated before we
/// tell the client to poll.
async fn await_hub<F: Fn(&[SessionInfo]) -> bool>(hub: &str, token: Option<&str>, pred: F) {
    for _ in 0..100 {
        if pred(&hub_sessions(hub, token).await) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    panic!("hub session list never satisfied the predicate");
}

// ── 1. Switcher lists sessions ──────────────────────────────────────────────

/// Proof of life: an app booted against a hub with two sessions renders both
/// names somewhere on screen (the start-in-grid action menu lists them).
#[tokio::test]
async fn boots_and_lists_sessions() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent: FakeAgentHandle =
        spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha"), sess("beta")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    assert!(
        h.pump_until(|t| t.contains("alpha") && t.contains("beta")).await,
        "both sessions should render; got:\n{}",
        h.text()
    );
}

/// The full-screen switcher (reached by clearing the only box) shows the session
/// names, their tool column, and the `N session(s) · <base-url>` breadcrumb.
#[tokio::test]
async fn switcher_shows_tool_and_breadcrumb() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha"), sess("beta")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Menu selection starts on the attached session (index 2). The attached box
    // adds a "Rename session" row (0059 C1) between the sessions and Clear, so with
    // 2 sessions Clear is at index 5 → Down×3. Clearing the only box → switcher.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;

    assert!(
        h.tick_until(|t| t.contains("alpha")
            && t.contains("beta")
            && t.contains("shell")
            && t.contains("session(s)"))
            .await,
        "switcher lists names + tool + breadcrumb; got:\n{}",
        h.text()
    );
}

// ── 2. Attach round-trip (snapshot + input echo) ────────────────────────────

#[tokio::test]
async fn attach_round_trip_shows_snapshot_and_echoes_input() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    h.key(KeyCode::Esc).await; // close the start-in-menu overlay → the pane shows

    // The scriptable agent answers the attach with an RIS snapshot whose body is
    // `SNAP:<machine>:<session>`.
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await,
        "attach snapshot should render; got:\n{}",
        h.text()
    );

    // A printable key rides the WS to the agent, which echoes it back as output.
    h.key(KeyCode::Char('x')).await;
    let a = agent.clone();
    assert!(
        h.pump_cond(move || a.observed().input.contains(&b'x')).await,
        "the agent should have received the typed 'x' through the hub"
    );
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alphax")).await,
        "the echoed 'x' should render after the snapshot; got:\n{}",
        h.text()
    );
}

// ── 2b. Direct attach (`ccs <session>`) ─────────────────────────────────────

/// 0059 C2: booting with a positional session query (`ccs al`, a unique prefix)
/// resolves to `alpha` and lands straight in the attached grid — the pane shows
/// the agent's snapshot and the action menu is NOT open (no "New session" row).
#[tokio::test]
async fn direct_attach_by_prefix_boots_into_grid() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent =
        spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha"), sess("beta")]).await;

    // "al" is a unique prefix of alpha (beta doesn't start with it).
    let mut h = Harness::boot_attached(&hub, 100, 30, "al").await;
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await,
        "direct-attach should boot straight into alpha's grid; got:\n{}",
        h.text()
    );
    // The action menu (its "New session" row) must NOT be showing — we skipped it.
    assert!(
        !h.text().contains("New session"),
        "direct attach must bypass the action menu; got:\n{}",
        h.text()
    );
}

// ── 3. Resize propagates ────────────────────────────────────────────────────

#[tokio::test]
async fn resize_propagates_to_agent() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    h.key(KeyCode::Esc).await;
    assert!(h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await, "attached");

    // Single layout is borderless, so the pane's inner size is the full body:
    // (cols, rows-1) — the last row is the app-owned status bar.
    h.resize(120, 40).await;
    let a = agent.clone();
    assert!(
        h.pump_cond(move || a.saw_resize(120, 39)).await,
        "agent should record the resize to the new pane size (120x39); saw: {:?}",
        agent.observed().resizes
    );
    assert!(!agent.observed().resizes.is_empty(), "at least one resize was propagated");
}

// ── 4. Reconnect replays (lighter check — see NOTE) ─────────────────────────

// NOTE: a *true* mid-test socket drop (killing the live client↔hub WS while the
// session survives) can't be forced deterministically from here without reaching
// into the hub's internals — the reconnect is driven by `ws::run`'s loop reacting
// to a `Close`/`Err` from tungstenite. What we CAN exercise deterministically is
// the substance of "reconnect replays": every (re)attach is answered with a fresh
// RIS snapshot that `Pane::process` uses to rebuild the emulator from scratch. So
// we attach, dirty the pane with echoed input, re-attach the same box, and assert
// the snapshot repaints (the transient bytes are gone) and the bar reaches "live"
// — the same RIS-repaint path a socket-level reconnect takes.
#[tokio::test]
async fn reattach_replays_snapshot() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    h.key(KeyCode::Esc).await;
    assert!(h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await, "first attach");

    // Dirty the pane: type 'Z', which the agent echoes into the emulator.
    h.key(KeyCode::Char('Z')).await;
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alphaZ")).await,
        "echoed 'Z' should be visible before re-attach; got:\n{}",
        h.text()
    );

    // Re-attach the same box via the action menu (^A d → Enter on the session).
    h.ctrl('a').await;
    h.key(KeyCode::Char('d')).await; // open menu (selection defaults to alpha)
    h.key(KeyCode::Enter).await; // re-attach box 0 to alpha

    // The fresh RIS snapshot repaints the pane: the 'Z' is gone, and the bar
    // reaches the live/connected state.
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alpha") && !t.contains("alphaZ") && t.contains("live"))
            .await,
        "re-attach should replay the snapshot and drop the transient 'Z'; got:\n{}",
        h.text()
    );
}

// ── 5. Session vanishes ⇒ auto-detach ───────────────────────────────────────

#[tokio::test]
async fn vanished_session_auto_detaches() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    h.key(KeyCode::Esc).await;
    assert!(h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await, "attached to alpha");

    // alpha disappears; beta takes its place (the agent re-advertises).
    agent.set_sessions(vec![sess("beta")]);
    await_hub(&hub, None, |list| {
        list.iter().any(|s| s.name == "beta") && !list.iter().any(|s| s.name == "alpha")
    })
    .await;

    // A poll refresh drops the box whose session ended; with no boxes left the app
    // falls back to the switcher, now listing only beta.
    assert!(
        h.tick_until(|t| t.contains("beta") && !t.contains("SNAP:boxA:alpha")).await,
        "the pane should auto-detach and the switcher list beta; got:\n{}",
        h.text()
    );
}

// ── 6. Create session ───────────────────────────────────────────────────────

#[tokio::test]
async fn create_session_lands_at_agent() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Menu starts on the attached session (index 2); Up → "New session" (index 1).
    h.key(KeyCode::Up).await;
    h.key(KeyCode::Enter).await; // open the inline new-session form (focus: Name)
    h.type_str("made").await; // type the session name
    h.key(KeyCode::Enter).await; // submit

    let a = agent.clone();
    assert!(
        h.pump_cond(move || {
            a.observed()
                .cmds
                .iter()
                .any(|c| matches!(c, Cmd::Create(r) if r.name == "made" && r.tool == "claude"))
        })
        .await,
        "a Create with the entered tool+name should reach the agent; saw: {:?}",
        agent.observed().cmds
    );

    // The double answers Create with `Created(name)` but doesn't itself
    // re-advertise its (now longer) list, so the hub poll wouldn't yet know
    // "made". Emulate the agent's post-create session poll, then assert the
    // client reconciles the new session into its list.
    agent.set_sessions(vec![sess("alpha"), sess("made")]);
    await_hub(&hub, None, |list| list.iter().any(|s| s.name == "made")).await;
    assert!(
        h.tick_until(|t| t.contains("made")).await,
        "the created session should subsequently list; got:\n{}",
        h.text()
    );
}

// ── 7. Kill with confirm ────────────────────────────────────────────────────

#[tokio::test]
async fn kill_with_confirm_deletes_and_drops_from_list() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha"), sess("beta")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Clear the only box to drop into the switcher. The attached box adds a Rename
    // row (0059 C1), so with 2 sessions Clear is at index 5 → Down×3.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;
    assert!(h.tick_until(|t| t.contains("alpha") && t.contains("beta")).await, "in switcher");

    // Kill the highlighted session (alpha) with the y/n confirm.
    h.key(KeyCode::Char('x')).await; // → confirm overlay
    assert!(h.pump_until(|t| t.contains("kill session alpha")).await, "confirm prompt shown");
    h.key(KeyCode::Char('y')).await; // confirm

    let a = agent.clone();
    assert!(
        h.pump_cond(move || {
            a.observed().cmds.iter().any(|c| matches!(c, Cmd::Delete(r) if r.session == "alpha"))
        })
        .await,
        "a Delete for alpha should reach the agent; saw: {:?}",
        agent.observed().cmds
    );

    // The double answers Delete with Ok but does not itself re-advertise its
    // (now shorter) list, so emulate the agent's post-kill session poll, then
    // assert the client reconciles alpha out of the list.
    agent.set_sessions(vec![sess("beta")]);
    await_hub(&hub, None, |list| !list.iter().any(|s| s.name == "alpha")).await;
    assert!(
        h.tick_until(|t| t.contains("beta") && !t.contains("alpha")).await,
        "alpha should leave the list after the kill; got:\n{}",
        h.text()
    );
}

// ── 8. Multi-machine (routing by machine) ───────────────────────────────────

#[tokio::test]
async fn multi_machine_lists_and_routes_by_machine() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _a = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;
    let _b = spawn_scriptable_agent(&hub, "boxB", None, vec![sess("bravo")]).await;
    await_hub(&hub, None, |list| {
        list.iter().any(|s| s.machine == "boxA") && list.iter().any(|s| s.machine == "boxB")
    })
    .await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Both sessions list in the boot menu, on both machines.
    assert!(
        h.pump_until(|t| t.contains("alpha") && t.contains("bravo")).await,
        "both machines' sessions list; got:\n{}",
        h.text()
    );
    h.key(KeyCode::Esc).await;
    // Box 0 attaches the first (alpha, on boxA).
    assert!(h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await, "box 0 routes to boxA");

    // Split into two boxes and attach bravo (boxB) into box 1.
    h.ctrl('a').await;
    h.key(KeyCode::Char('3')).await; // layout 3 = side-by-side
    h.ctrl('a').await;
    h.key(KeyCode::Right).await; // focus box 1
    h.key(KeyCode::Enter).await; // empty box → open its menu
    h.key(KeyCode::Down).await; // move from alpha (idx 2) to bravo (idx 3)
    h.key(KeyCode::Enter).await; // attach bravo

    assert!(
        h.pump_until(|t| t.contains("SNAP:boxB:bravo")).await,
        "box 1 routes to boxB; got:\n{}",
        h.text()
    );
    // Bordered layout titles carry the machine tag (machine/session).
    let t = h.text();
    assert!(t.contains("boxA/alpha"), "box 0 titled by machine; got:\n{t}");
    assert!(t.contains("boxB/bravo"), "box 1 titled by machine; got:\n{t}");
    assert!(t.contains("SNAP:boxA:alpha"), "box 0 still shows boxA's snapshot; got:\n{t}");
}

// ── 9. Auth ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_without_token_surfaces_actionable_error() {
    let hub = start_hub(Auth::new(Some("pw".into()), Some("tok".into()), [7u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    // No token → the first `/api/sessions` poll 401s, which the client turns into
    // an actionable "requires auth / pass --token" status (not a terse 401).
    let mut h = Harness::boot(&hub, 100, 30).await;
    // Give the initial poll a beat to have run (it's part of `init`).
    h.pump_once().await;
    assert!(
        h.app.status().contains("requires auth"),
        "auth-gated boot without a token should surface an auth error; status: {:?}",
        h.app.status()
    );
}

#[tokio::test]
async fn auth_with_token_lists_and_attaches() {
    let hub = start_hub(Auth::new(Some("pw".into()), Some("tok".into()), [7u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    // The correct API token unlocks the same flows as an unauthenticated hub.
    let mut h = Harness::boot_auth(&hub, 100, 30, Some("tok".into())).await;
    assert!(
        h.pump_until(|t| t.contains("alpha")).await,
        "with a token, sessions list; got:\n{}",
        h.text()
    );
    h.key(KeyCode::Esc).await;
    assert!(
        h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await,
        "with a token, attach works (WS handshake carries the bearer); got:\n{}",
        h.text()
    );
}

// ── C4. Switcher shows the LLM headline + web-set colour accent ──────────────

/// A session carrying a web-set `headline` (the LLM ≤6-word summary) and a
/// `color` mark renders both in the switcher: the headline as dim trailing text,
/// the mark colour as the attach-dot accent (display-only — the TUI never sets
/// either). We drop into the full-screen switcher (clear the only box), assert
/// the headline text is on screen, and assert a dot cell is painted the mapped
/// `teal` RGB (`TestBackend` text can't show colour, so we read cell *style*).
#[tokio::test]
async fn switcher_shows_headline_and_color_accent() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let mut alpha = sess("alpha");
    alpha.headline = Some("fix the parser".into());
    alpha.color = Some("teal".into()); // curated palette token (proposal 0029)
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![alpha, sess("beta")]).await;

    // Make the poll deterministic: wait until the hub advertises alpha (with its
    // headline/colour) before the client's boot poll runs.
    await_hub(&hub, None, |list| list.iter().any(|s| s.name == "alpha")).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Menu starts on the attached session; the focused box holds a session, so the
    // action menu carries a Rename row (0059 C1) — with 2 sessions "Clear this box"
    // sits at Down×3. Clearing the only box falls back to the full-screen switcher.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;

    // The switcher row surfaces the LLM headline as trailing text.
    assert!(
        h.tick_until(|t| t.contains("alpha") && t.contains("fix the parser")).await,
        "switcher should render the headline; got:\n{}",
        h.text()
    );

    // The attach dot for the marked session is painted the mapped teal (the web's
    // `hsl(175 60% 58%)` → Rgb(84,212,201)). Scan for a dot cell in that colour.
    let teal = Color::Rgb(84, 212, 201);
    let buf = h.term.backend().buffer();
    let area = buf.area;
    let mut found = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            if (cell.symbol() == "●" || cell.symbol() == "○") && cell.style().fg == Some(teal) {
                found = true;
            }
        }
    }
    assert!(found, "a teal-accented attach dot should be painted; got:\n{}", h.text());
}

// ── keyboard scrollback (0059 C3) ───────────────────────────────────────────

/// `^A [` enters keyboard-scroll mode on the focused pane; PgUp pages back into
/// real scrollback (the fake agent echoes input as output, so many `Ctrl+J`
/// line-feeds build >1 screen of history), the statusbar's `⇡ scrollback N`
/// indicator tracks the offset, and `q` snaps back to live (offset 0, indicator
/// gone). Input never reaches the encoder while scrolling.
#[tokio::test]
async fn keyboard_scrollback_pages_and_returns_to_live() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    // A short pane (rows-1 == 9) so a handful of line-feeds already overflows one
    // screen and spills into scrollback. Wide enough that the left `scrollback`
    // indicator and the right-aligned scroll-key hint don't overlap in the bar.
    let mut h = Harness::boot(&hub, 100, 10).await;
    h.key(KeyCode::Esc).await; // close the start menu → the pane shows
    assert!(h.pump_until(|t| t.contains("SNAP:boxA:alpha")).await, "attached to alpha");

    // Build >1 screen of history: 30 numbered lines. Ctrl+J encodes to `\n`
    // (0x0a), which the agent echoes back as output; each line-feed past the
    // bottom pushes a line into alacritty's scrollback.
    for i in 0..30 {
        h.type_str(&format!("LN{i}")).await;
        h.ctrl('j').await; // line feed
    }
    // Wait until the newest line has been echoed and rendered (round-trip done).
    assert!(
        h.pump_until(|t| t.contains("LN29")).await,
        "the echoed lines should render live; got:\n{}",
        h.text()
    );
    // Live: no scrollback indicator yet.
    assert!(!h.text().contains("scrollback"), "live view has no indicator; got:\n{}", h.text());

    // Enter scroll mode: ^A [ . The indicator appears (marks the mode) even before
    // paging.
    h.ctrl('a').await;
    h.key(KeyCode::Char('[')).await;
    assert!(
        h.pump_until(|t| t.contains("scrollback")).await,
        "entering scroll mode shows the ⇡ scrollback indicator; got:\n{}",
        h.text()
    );
    // Just entered — still at the live bottom, so the indicator reads offset 0.
    assert!(h.text().contains("scrollback 0"), "scroll mode starts live; got:\n{}", h.text());
    let live_view = h.text();

    // PgUp pages back a full screen: the offset climbs above 0 (no longer
    // `scrollback 0`) and the view changes (the newest line scrolls out).
    h.key(KeyCode::PageUp).await;
    assert!(
        h.pump_until(|t| t.contains("scrollback") && !t.contains("scrollback 0")).await,
        "PgUp pages back — the offset should climb above 0; got:\n{}",
        h.text()
    );
    let paged_view = h.text();
    assert_ne!(live_view, paged_view, "the paged view differs from the live view");
    assert!(!paged_view.contains("LN29"), "the newest line scrolled off; got:\n{paged_view}");

    // `q` returns to live: the offset resets to 0 and the indicator disappears.
    h.key(KeyCode::Char('q')).await;
    assert!(
        h.pump_until(|t| !t.contains("scrollback") && t.contains("LN29")).await,
        "q snaps back to the live bottom (indicator gone, newest line shown); got:\n{}",
        h.text()
    );
}

// ── 10. Rename a session (0059 C1) ──────────────────────────────────────────

/// Switcher `R` on a non-empty list renames the selected session: type a label,
/// Enter → a `Cmd::SetLabel{label:Some(..)}` reaches the agent; after the agent's
/// next poll advertises the relabelled session, the switcher row shows the label.
#[tokio::test]
async fn rename_session_sets_label_and_row_reflects_it() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha"), sess("beta")]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // Drop into the full-screen switcher (clear the only box). Attached box adds a
    // Rename row, so with 2 sessions Clear is at index 5 → Down×3.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;
    assert!(h.tick_until(|t| t.contains("alpha") && t.contains("beta")).await, "in switcher");

    // Selection starts on the first row (alpha). R → rename overlay, type a label.
    h.key(KeyCode::Char('R')).await;
    assert!(h.pump_until(|t| t.contains("rename session")).await, "rename overlay shown");
    // The field is seeded with the current display name ("alpha") — clear it, then
    // type the new label.
    for _ in 0..10 {
        h.key(KeyCode::Backspace).await;
    }
    h.type_str("My Feature").await;
    h.key(KeyCode::Enter).await;

    // A SetLabel for alpha with the typed label reached the agent through the hub.
    let a = agent.clone();
    assert!(
        h.pump_cond(move || {
            a.observed().cmds.iter().any(|c| {
                matches!(c, Cmd::SetLabel { session, label }
                    if session == "alpha" && label.as_deref() == Some("My Feature"))
            })
        })
        .await,
        "a SetLabel{{alpha, Some(\"My Feature\")}} should reach the agent; saw: {:?}",
        agent.observed().cmds
    );

    // Emulate the agent's post-rename poll (it mutated its copy but doesn't auto-push
    // a fresh Sessions), then assert the switcher row shows the display label.
    let mut relabelled = sess("alpha");
    relabelled.label = Some("My Feature".into());
    agent.set_sessions(vec![relabelled, sess("beta")]);
    await_hub(&hub, None, |list| {
        list.iter().any(|s| s.name == "alpha" && s.label.as_deref() == Some("My Feature"))
    })
    .await;
    assert!(
        h.tick_until(|t| t.contains("My Feature")).await,
        "the switcher row should render the new display label; got:\n{}",
        h.text()
    );
}

/// Submitting an empty rename clears the label — the client sends `label: None`.
#[tokio::test]
async fn rename_session_empty_value_clears_label() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    // Boot with a session that already carries a label, so clearing is meaningful.
    let mut labelled = sess("alpha");
    labelled.label = Some("Old Label".into());
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![labelled]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // 1 session, attached box → menu is [layout, new, alpha(2), rename(3), clear(4),
    // quit]; from the initial alpha selection Down×2 reaches Clear → switcher.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;
    assert!(h.tick_until(|t| t.contains("Old Label")).await, "switcher shows the current label");

    h.key(KeyCode::Char('R')).await;
    assert!(h.pump_until(|t| t.contains("rename session")).await, "rename overlay shown");
    // The field is seeded with "Old Label" — clear it, then submit empty.
    for _ in 0..20 {
        h.key(KeyCode::Backspace).await;
    }
    h.key(KeyCode::Enter).await;

    let a = agent.clone();
    assert!(
        h.pump_cond(move || {
            a.observed().cmds.iter().any(|c| {
                matches!(c, Cmd::SetLabel { session, label } if session == "alpha" && label.is_none())
            })
        })
        .await,
        "an empty submit should send SetLabel{{alpha, None}}; saw: {:?}",
        agent.observed().cmds
    );
}

// ── 11. Restorable-session picker (0059 C5) ─────────────────────────────────

/// Switcher `R` on an EMPTY session list opens the restorable picker: it fetches
/// the restorable set (a `Cmd::Restorable` reaches the agent), lists the two
/// restorable sessions, and Enter fires an all-or-nothing `Cmd::Restore`.
#[tokio::test]
async fn restorable_picker_lists_and_restores() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    // No live sessions → the switcher is empty, so R opens the picker (not rename).
    let agent = spawn_scriptable_agent(&hub, "boxA", None, vec![]).await;

    let mut h = Harness::boot(&hub, 100, 30).await;
    // With no sessions the app boots into the grid with the action menu open on an
    // empty box (menu_initial → New session). Down → "Clear this box"; Enter clears
    // the only box, which falls back to the (empty) full-screen switcher.
    h.key(KeyCode::Down).await;
    h.key(KeyCode::Enter).await;
    assert!(
        h.tick_until(|t| t.contains("no sessions")).await,
        "reach the empty switcher; got:\n{}",
        h.text()
    );

    // R on the empty list → fetch restorable + open the picker.
    h.key(KeyCode::Char('R')).await;
    assert!(
        h.pump_until(|t| t.contains("restore-me") && t.contains("restore-too")).await,
        "the restorable picker should list both restorable sessions; got:\n{}",
        h.text()
    );
    // The fetch was a Cmd::Restorable routed to the agent.
    let a = agent.clone();
    assert!(
        h.pump_cond(move || a.observed().cmds.iter().any(|c| matches!(c, Cmd::Restorable))).await,
        "a Restorable probe should reach the agent; saw: {:?}",
        agent.observed().cmds
    );

    // Enter restores (all-or-nothing) → a Cmd::Restore reaches the agent.
    h.key(KeyCode::Enter).await;
    let a = agent.clone();
    assert!(
        h.pump_cond(move || a.observed().cmds.iter().any(|c| matches!(c, Cmd::Restore))).await,
        "Enter in the picker should fire an all-or-nothing Restore; saw: {:?}",
        agent.observed().cmds
    );
}

// ── narrow-terminal boot (terminal-environments note) ───────────────────────

#[tokio::test]
async fn narrow_boot_lists_without_panic() {
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    // A 40x15 terminal must still render the menu/session list without panicking.
    let mut h = Harness::boot(&hub, 40, 15).await;
    assert!(
        h.pump_until(|t| t.contains("alpha")).await,
        "narrow boot should list the session; got:\n{}",
        h.text()
    );
}

// ── proposal 0060: `ccs activate` — device sign-in against the SaaS hub ───────
//
// These drive the real activate poll loop (`cc_screen_tui::actions`) against
// the real multi-tenant hub router (`start_hub_multi`), with the "browser side"
// played by the harness (scrape the pending code, POST approve with a login
// cookie) — the exact shape scripts/e2e-saas-journey.sh proves against prod.

use cc_screen_hub::test_support::{start_hub_multi, MultiHub};
use cc_screen_tui::actions::{self, ActivateError};

fn base_url(hub: &MultiHub) -> String {
    format!("http://{}", hub.addr)
}

/// The happy path (acceptance 1): code → approve from a "phone" → the token
/// lands, the email is echoed, and the Bearer then sees exactly the user's own
/// scope — while the other tenant's world stays invisible.
#[tokio::test]
async fn activate_happy_path_scopes_and_revokes() {
    let hub = start_hub_multi().await;
    let (_alice, alice_cookie) = hub.user_with_cookie("alice@x.com").await;

    // Browser side: wait for the code, approve it.
    let approver = {
        let hub = hub.clone();
        let cookie = alice_cookie.clone();
        tokio::spawn(async move {
            let code = hub.pending_user_code().await.expect("a pending code appears");
            assert_eq!(hub.approve(&cookie, &code).await, 200, "approve succeeds");
        })
    };

    // Terminal side: the real poll loop (no browser auto-open in tests).
    let mut out = Vec::new();
    let a = actions::run_activate(&base_url(&hub), false, "alice@testbox", false, &mut out)
        .await
        .expect("activate completes");
    approver.await.unwrap();
    assert_eq!(a.email, "alice@x.com", "the account email is echoed");
    let printed = String::from_utf8_lossy(&out);
    // The code prints BEFORE the URL, and the token itself never prints.
    let code_pos = printed.find("one-time code").expect("code line");
    let url_pos = printed.find("/activate").expect("url line");
    assert!(code_pos < url_pos, "code before URL:\n{printed}");
    assert!(!printed.contains(&a.token), "the token never reaches the screen");

    // The minted Bearer lights up the client surface, scoped to alice: bob's
    // registered machine must be invisible (acceptance 7).
    let (bob, bob_cookie) = hub.user_with_cookie("bob@x.com").await;
    let uplink = {
        use cc_screen_hub::db::Store as _;
        let (t, _aid) = hub.store.upsert_agent(&bob, "bobbox").await.unwrap();
        t
    };
    spawn_scriptable_agent(&hub.addr, "bobbox", Some(&uplink), vec![sess("beta")]).await;
    let http = reqwest::Client::new();
    // Wait until bob himself can see his machine (cookie view), so the uplink
    // is definitely registered before asserting alice's blindness.
    let mut bob_sees = false;
    for _ in 0..100 {
        let v: serde_json::Value = http
            .get(format!("{}/api/machines", base_url(&hub)))
            .header("cookie", &bob_cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if v.as_array().is_some_and(|a| !a.is_empty()) {
            bob_sees = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(bob_sees, "bob's agent registers");
    let alice_view: serde_json::Value = http
        .get(format!("{}/api/machines", base_url(&hub)))
        .bearer_auth(&a.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_view.as_array().map(|v| v.len()), Some(0), "alice's token can't see bob's machine");

    // `ccs logout`'s server half: self-revoke, then the old token 401s.
    let revoke = http
        .post(format!("{}/api/client-tokens/revoke-self", base_url(&hub)))
        .bearer_auth(&a.token)
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success());
    let after = http
        .get(format!("{}/api/sessions", base_url(&hub)))
        .bearer_auth(&a.token)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status().as_u16(), 401, "revocation is immediate");
}

/// A client token presented on the agent uplink must never register a machine
/// (the other half of acceptance 7's cross-credential test).
#[tokio::test]
async fn client_token_rejected_on_agent_uplink() {
    let hub = start_hub_multi().await;
    let (_alice, cookie) = hub.user_with_cookie("alice@x.com").await;

    // Mint a client token via the raw wire (fast path, no poll loop).
    let http = reqwest::Client::new();
    let code: serde_json::Value = http
        .post(format!("{}/api/device/client/code", base_url(&hub)))
        .json(&serde_json::json!({ "label": "x" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hub.approve(&cookie, code["user_code"].as_str().unwrap()).await, 200);
    let tok: serde_json::Value = http
        .post(format!("{}/api/device/client/token", base_url(&hub)))
        .json(&serde_json::json!({ "device_code": code["device_code"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_token = tok["client_token"].as_str().unwrap();

    // Present it as an uplink credential: registration must not land.
    use cc_screen_hub::test_support::{connect, send};
    use cc_screen_protocol::hub::{AgentMsg, HUB_PROTO_VERSION};
    if let Ok(mut ws) = connect(&format!("ws://{}/agent/ws", hub.addr), Some(client_token)).await {
        send(
            &mut ws,
            &AgentMsg::Register {
                proto: HUB_PROTO_VERSION,
                machine_id: "ghost".into(),
                hostname: "ghost".into(),
                agent_version: "test".into(),
                tools: vec![],
                caps: vec![],
            },
            b"",
        )
        .await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let v: serde_json::Value = http
        .get(format!("{}/api/machines", base_url(&hub)))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v.as_array().map(|a| a.len()), Some(0), "a client token never registers a machine");
}

/// Denied and expired codes end the flow with the clean exit-1 errors (C2/C4).
#[tokio::test]
async fn activate_denied_and_expired_paths() {
    // Denied.
    let hub = start_hub_multi().await;
    let denier = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.deny_pending().await })
    };
    let mut out = Vec::new();
    let err = actions::run_activate(&base_url(&hub), false, "x", false, &mut out)
        .await
        .map(|_| ())
        .expect_err("denied flow errors");
    denier.await.unwrap();
    assert!(matches!(err, ActivateError::Denied), "got {err:?}");
    assert_eq!(err.exit_code(), 1);

    // Expired.
    let hub = start_hub_multi().await;
    let expirer = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.expire_pending().await })
    };
    let mut out = Vec::new();
    let err = actions::run_activate(&base_url(&hub), false, "x", false, &mut out)
        .await
        .map(|_| ())
        .expect_err("expired flow errors");
    expirer.await.unwrap();
    assert!(matches!(err, ActivateError::Expired), "got {err:?}");
    assert!(err.message().contains("ccs activate"), "expiry names the retry command");
}

/// The degradation matrix (C4/acceptance 6): a single-tenant hub says
/// "static token" (exit 2); a pre-0060 multi-tenant hub says "too old" (exit 2).
#[tokio::test]
async fn activate_degrades_on_single_tenant_and_old_hubs() {
    // Single-tenant: the /api/me probe short-circuits before any device call.
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let mut out = Vec::new();
    let err = actions::run_activate(&format!("http://{hub}"), false, "x", false, &mut out)
        .await
        .map(|_| ())
        .expect_err("single-tenant refuses");
    assert!(matches!(err, ActivateError::SingleTenant), "got {err:?}");
    assert_eq!(err.exit_code(), 2);
    assert!(err.message().contains("api_token"), "points at the static-token path");

    // Pre-0060 multi-tenant hub: multiTenant:true but no client device routes.
    let app = axum::Router::new().route(
        "/api/me",
        axum::routing::get(|| async { axum::Json(serde_json::json!({ "multiTenant": true })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut out = Vec::new();
    let err = actions::run_activate(&format!("http://{addr}"), false, "x", false, &mut out)
        .await
        .map(|_| ())
        .expect_err("pre-0060 hub refuses");
    assert!(matches!(err, ActivateError::HubTooOld), "got {err:?}");
    assert_eq!(err.exit_code(), 2);
    assert!(err.message().contains("update"), "points at updating the hub");
}
