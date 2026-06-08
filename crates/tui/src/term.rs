//! Terminal setup/teardown with a panic hook so a crash never leaves the user's
//! shell in raw mode / on the alternate screen.

use std::io::{self, Stdout, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::Terminal;

use crate::anchored_backend::AnchoredBackend;

pub type Tui = Terminal<AnchoredBackend<Stdout>>;

/// Enter raw mode + the alternate screen and return a ratatui terminal. Installs
/// a panic hook that restores the terminal first, so a panic's backtrace lands
/// on a sane screen.
pub fn enter() -> Result<Tui> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // Mouse capture lets the wheel drive the pane's scrollback. It disables the
    // terminal's own click-to-select, but Shift+drag still selects on every
    // common terminal (the bytes bypass mouse reporting). Focus reporting
    // (DECSET 1004) drives the 0018 foreground/background notification split:
    // toast when focused, bell + OSC 9 when not.
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        EnableFocusChange,
        cursor::Hide
    )?;
    install_panic_hook();
    Ok(Terminal::new(AnchoredBackend::new(out))?)
}

/// Undo `enter()`. Safe to call more than once.
pub fn restore() -> Result<()> {
    let mut out = io::stdout();
    execute!(
        out,
        cursor::Show,
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    out.flush()?;
    Ok(())
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        prev(info);
    }));
}
