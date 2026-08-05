//! End-to-end tests for the `ccs` TUI (proposal 0059 B3).
//!
//! Each test starts an in-process hub + one or more fake agents (the SAME
//! wire-contract doubles the hub's own suite uses, hoisted into
//! `cc_screen_hub::test_support` by B1), builds a real `App` pointed at the hub's
//! ephemeral port, drives it with synthetic `AppMsg`s, renders into a
//! `TestBackend`, and asserts on the buffer text. No real terminal, no sleeps for
//! correctness (we wait on actual channel messages with a generous timeout), no
//! docker.

use std::time::Duration;

use cc_screen_auth::Auth;
use cc_screen_hub::test_support::{sess, spawn_scriptable_agent, start_hub, FakeAgentHandle};
use cc_screen_tui::app::{App, AppMsg, RunOpts};
use cc_screen_tui::client::Rest;
use cc_screen_tui::config::Config;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

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
        let rest = Rest::new(&format!("http://{hub}"), false, token).expect("rest");
        let mut app = App::new(rest, Config::default());
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

    /// Flatten the `TestBackend` buffer into a single string (row by row).
    fn text(&self) -> String {
        let buf = self.term.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }
}

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
