//! B4 PTY smoke test (proposal 0059) — the flaky tier, quarantined behind the
//! `pty-smoke` feature so it never blocks the main `cargo test -p cc-screen-tui`
//! run. CI runs it as its own isolated step:
//!
//!     cargo test -p cc-screen-tui --features pty-smoke --test pty_smoke
//!
//! Unlike the `tests/e2e.rs` harness (which drives an in-process `App` against a
//! `TestBackend`), this boots the REAL `ccs` binary inside a `portable-pty` pair,
//! pointed at an in-process hub + fake agent, drives it over the pty like a human
//! would, and reads the child's screen back by feeding its raw output through an
//! `alacritty_terminal` emulator (never regexing raw bytes). It proves the end-to
//! -end binary path: process boots → attach shows the agent's snapshot → quit
//! exits cleanly and leaves the alternate screen.
#![cfg(feature = "pty-smoke")]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;
use cc_screen_auth::Auth;
use cc_screen_hub::test_support::{sess, spawn_scriptable_agent, start_hub};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// A fixed emulator size satisfying alacritty's `Dimensions` (no scrollback).
struct Size {
    cols: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Decode a raw pty byte stream into the visible screen text via alacritty.
fn decode(bytes: &[u8], cols: u16, rows: u16) -> String {
    let size = Size { cols: cols as usize, rows: rows as usize };
    let mut term = Term::new(TermConfig::default(), &size, VoidListener);
    let mut p: Processor = Processor::new();
    p.advance(&mut term, bytes);
    let grid = term.grid();
    let mut out = String::new();
    for l in 0..rows as i32 {
        for c in 0..cols as usize {
            out.push(grid[Line(l)][Column(c)].c);
        }
        out.push('\n');
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ccs_boots_attaches_and_quits() {
    const COLS: u16 = 100;
    const ROWS: u16 = 30;

    // In-process hub + a scriptable fake agent owning one session.
    let hub = start_hub(Auth::new(None, None, [0u8; 32]), &[]).await;
    let _agent = spawn_scriptable_agent(&hub, "boxA", None, vec![sess("alpha")]).await;

    // Open a pty and spawn the real `ccs` binary into it.
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: ROWS, cols: COLS, pixel_width: 0, pixel_height: 0 })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_ccs"));
    cmd.arg("--server");
    cmd.arg(format!("http://{hub}"));
    // Keep the child's config/state off the developer's real home.
    let cfg_dir = std::env::temp_dir().join(format!("ccs-pty-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&cfg_dir);
    cmd.env("XDG_CONFIG_HOME", &cfg_dir);
    cmd.env("HOME", &cfg_dir);
    cmd.env("TERM", "xterm-256color");
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn ccs");
    drop(pair.slave); // we don't need the slave handle anymore

    // Reader thread: accumulate the child's output into a shared buffer so the
    // async test body can poll the decoded screen without blocking the runtime.
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let mut reader = pair.master.try_clone_reader().expect("reader");
    {
        let buf = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });
    }
    let mut writer = pair.master.take_writer().expect("writer");

    // Retry-until-match on the decoded screen (generous budget: the child has to
    // build its HTTP client, poll the hub, attach the WS, and repaint).
    let screen_has = |needle: &str| -> bool {
        let bytes = buf.lock().unwrap().clone();
        decode(&bytes, COLS, ROWS).contains(needle)
    };
    let wait_for = |needle: &'static str| async {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if screen_has(needle) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        screen_has(needle)
    };

    // The app boots straight into the grid with the action menu over box 0, which
    // is attached to the first session — so its snapshot ("SNAP:boxA:alpha" from
    // the scriptable agent) is on screen.
    assert!(
        wait_for("SNAP:boxA:alpha").await,
        "ccs should attach and render the agent snapshot; screen:\n{}",
        decode(&buf.lock().unwrap().clone(), COLS, ROWS)
    );

    // Quit through the action menu. The focused box holds a session, so the menu
    // carries a Rename row (0059 C1): [Change layout, New session, alpha, Rename,
    // Clear this box, Quit]. Selection starts on the attached session (alpha), so
    // Down×3 highlights "Quit ccs"; Enter selects it.
    writer.write_all(b"\x1b[B\x1b[B\x1b[B\r").expect("write quit keys");
    writer.flush().ok();

    // The process exits cleanly.
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("ccs did not exit after the Quit menu action");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(status.success(), "ccs exited non-zero: {status:?}");

    // On teardown the TUI leaves the alternate screen — a control sequence, so we
    // look at the raw byte stream for it (not the decoded text).
    let raw = buf.lock().unwrap().clone();
    assert!(
        raw.windows(8).any(|w| w == b"\x1b[?1049l"),
        "the alt-screen leave sequence should appear in the raw output on quit"
    );
}
