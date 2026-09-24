//! Terminal setup/teardown with a panic hook so a crash never leaves the user's
//! shell in raw mode / on the alternate screen.

use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};

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
    // Save the user's window title (XTWINOPS 22) so `restore` can put it back
    // after `set_title` has named the focused session. Emulators without a
    // title stack ignore it — then the last session name simply lingers.
    let _ = write!(out, "\x1b[22;0t");
    let _ = out.flush();
    TITLE_PUSHED.store(true, Ordering::SeqCst);
    install_panic_hook();
    Ok(Terminal::new(AnchoredBackend::new(out))?)
}

/// Undo `enter()`. Safe to call more than once.
pub fn restore() -> Result<()> {
    let mut out = io::stdout();
    // Pop only what `enter` pushed — `restore` may run twice (panic hook + exit).
    if TITLE_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = write!(out, "\x1b[23;0t");
    }
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

/// Whether `enter` pushed the user's title, i.e. `restore` owes one pop.
static TITLE_PUSHED: AtomicBool = AtomicBool::new(false);

/// OSC 2 window/tab title. Control characters are stripped so a session label
/// can never smuggle a terminator (and with it another sequence) out.
pub fn write_title<W: Write>(out: &mut W, title: &str) -> io::Result<()> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    write!(out, "\x1b]2;{clean}\x07")?;
    out.flush()
}

/// `write_title` onto the real stdout, between frames.
pub fn set_title(title: &str) {
    let _ = write_title(&mut io::stdout(), title);
}

/// Write one OSC 52 clipboard-store for `text` (cc-screen-saas proposal 0077
/// Part C).
///
/// This is how a copy performed INSIDE a session reaches the machine the user
/// is actually sitting at: `ccs` re-emits the sequence on its own stdout and the
/// user's terminal emulator (iTerm2, kitty, WezTerm, Windows Terminal, …)
/// performs the write. `ccs` deliberately does not drive a clipboard crate
/// itself — a `ccs` running over SSH on a headless box has no display server,
/// while its outer terminal does have a clipboard and already implements OSC 52.
///
/// The sequence is RE-ENCODED from the decoded, sanitised `String` — never a
/// forwarding of received bytes — so nothing the session sent can ride along.
/// Emitting nothing is the correct behaviour for an emulator that doesn't
/// support OSC 52: it consumes the sequence and nothing happens, rather than
/// spraying escape-sequence garbage into the pane.
pub fn write_osc52<W: Write>(out: &mut W, text: &str) -> io::Result<()> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    write!(out, "\x1b]52;c;{b64}\x07")?;
    out.flush()
}

/// `write_osc52` onto the real stdout. Called BETWEEN frames — the sequence
/// moves no cursor and changes no screen state, so it cannot corrupt the frame
/// ratatui just drew, but it must not be interleaved INTO one either.
pub fn emit_osc52(text: &str) {
    let mut out = io::stdout();
    let _ = write_osc52(&mut out, text);
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_strips_control_characters() {
        let mut out = Vec::new();
        write_title(&mut out, "api\x07\x1b]52;c;evil\x07 · ccs").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b]2;api]52;c;evil · ccs\x07");
    }
}
