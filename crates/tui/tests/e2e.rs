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
    // Menu selection starts on the attached session (index 2); Down×2 → "Clear
    // this box" (index 2 + n = 4). Clearing the only box falls back to the switcher.
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
    // Clear the only box (Down×2 → "Clear this box") to drop into the switcher.
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
    // Menu starts on the attached session (index 2); Down×2 → "Clear this box"
    // (index 2 + 2 sessions). Clearing the only box falls back to the switcher.
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
