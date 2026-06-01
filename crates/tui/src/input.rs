//! Encode a crossterm `KeyEvent` into the VT byte sequence a terminal app
//! expects on stdin. This is the half passthrough gave us for free; here it's a
//! bounded, fully-testable table.
//!
//! Notable cases:
//! - cursor keys honour the app's DECCKM (application-cursor) mode
//! - modified specials use xterm's `CSI 1 ; <mod> <final>` form
//! - Ctrl+letter folds to the C0 control byte; Alt prefixes ESC

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Returns the bytes to send for `key`, or `None` if it has no stdin encoding
/// (bare modifier presses, unmapped keys). `app_cursor` is the emulator's DECCKM
/// state (`screen.application_cursor()`).
pub fn encode(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let alt = m.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if m.contains(KeyModifiers::CONTROL) {
                match ctrl_byte(c) {
                    Some(b) => out.push(b),
                    None => push_char(&mut out, c), // e.g. Ctrl+digit: send the char
                }
            } else {
                push_char(&mut out, c);
            }
            Some(out)
        }

        KeyCode::Enter => Some(with_alt(alt, vec![b'\r'])),
        KeyCode::Esc => Some(with_alt(alt, vec![0x1b])),
        KeyCode::Tab => Some(with_alt(alt, vec![b'\t'])),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => {
            // Ctrl+Backspace -> ^H (0x08); plain -> DEL (0x7f); Alt prefixes ESC.
            let base = if m.contains(KeyModifiers::CONTROL) { 0x08 } else { 0x7f };
            Some(with_alt(alt, vec![base]))
        }

        // Cursor keys (final byte A/B/C/D/H/F) — DECCKM- and modifier-aware.
        KeyCode::Up => cursor_key(b'A', m, app_cursor),
        KeyCode::Down => cursor_key(b'B', m, app_cursor),
        KeyCode::Right => cursor_key(b'C', m, app_cursor),
        KeyCode::Left => cursor_key(b'D', m, app_cursor),
        KeyCode::Home => cursor_key(b'H', m, app_cursor),
        KeyCode::End => cursor_key(b'F', m, app_cursor),

        // Tilde keys.
        KeyCode::Insert => Some(csi_tilde(2, m)),
        KeyCode::Delete => Some(csi_tilde(3, m)),
        KeyCode::PageUp => Some(csi_tilde(5, m)),
        KeyCode::PageDown => Some(csi_tilde(6, m)),

        KeyCode::F(n) => fkey(n, m),

        _ => None,
    }
}

/// Parse a prefix-key spec like `"C-a"` (ctrl), `"M-x"`/`"A-x"` (alt), or a bare
/// char. A bare char with no modifier defaults to Ctrl (a printable prefix would
/// be unusable). Returns the `(code, modifiers)` to match against key events.
pub fn parse_prefix(s: &str) -> (KeyCode, KeyModifiers) {
    let mut mods = KeyModifiers::empty();
    let mut rest = s.trim();
    loop {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("c-") {
            mods |= KeyModifiers::CONTROL;
            rest = &rest[2..];
        } else if lower.starts_with("m-") || lower.starts_with("a-") {
            mods |= KeyModifiers::ALT;
            rest = &rest[2..];
        } else {
            break;
        }
    }
    let code = rest
        .chars()
        .next()
        .map(|c| KeyCode::Char(c.to_ascii_lowercase()))
        .unwrap_or(KeyCode::Char('a'));
    if mods.is_empty() {
        mods = KeyModifiers::CONTROL;
    }
    (code, mods)
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

fn with_alt(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// C0 control byte for Ctrl+<c>, per xterm: `@A-Z[\]^_` (0x40–0x5f) fold to
/// 0x00–0x1f (letters case-insensitive), plus Ctrl+Space=NUL and Ctrl+?=DEL.
fn ctrl_byte(c: char) -> Option<u8> {
    let up = c.to_ascii_uppercase() as u32;
    match up {
        0x40..=0x5F => Some((up & 0x1f) as u8),
        0x20 => Some(0x00), // Ctrl+Space
        0x3F => Some(0x7f), // Ctrl+?
        _ => None,
    }
}

/// xterm modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
fn mod_code(m: KeyModifiers) -> u8 {
    1 + (m.contains(KeyModifiers::SHIFT) as u8)
        + (m.contains(KeyModifiers::ALT) as u8) * 2
        + (m.contains(KeyModifiers::CONTROL) as u8) * 4
}

fn cursor_key(final_byte: u8, m: KeyModifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let code = mod_code(m);
    if code == 1 {
        // Unmodified: SS3 (ESC O x) in application-cursor mode, else CSI (ESC [ x).
        let intro = if app_cursor { b'O' } else { b'[' };
        Some(vec![0x1b, intro, final_byte])
    } else {
        Some(format!("\x1b[1;{code}{}", final_byte as char).into_bytes())
    }
}

fn csi_tilde(n: u8, m: KeyModifiers) -> Vec<u8> {
    let code = mod_code(m);
    if code == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{code}~").into_bytes()
    }
}

fn fkey(n: u8, m: KeyModifiers) -> Option<Vec<u8>> {
    let code = mod_code(m);
    match n {
        1..=4 => {
            let f = [b'P', b'Q', b'R', b'S'][(n - 1) as usize];
            Some(if code == 1 {
                vec![0x1b, b'O', f]
            } else {
                format!("\x1b[1;{code}{}", f as char).into_bytes()
            })
        }
        5 => Some(csi_tilde(15, m)),
        6 => Some(csi_tilde(17, m)),
        7 => Some(csi_tilde(18, m)),
        8 => Some(csi_tilde(19, m)),
        9 => Some(csi_tilde(20, m)),
        10 => Some(csi_tilde(21, m)),
        11 => Some(csi_tilde(23, m)),
        12 => Some(csi_tilde(24, m)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
    const NONE: KeyModifiers = KeyModifiers::empty();
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

    #[test]
    fn plain_and_unicode_chars() {
        assert_eq!(encode(k(KeyCode::Char('a'), NONE), false).unwrap(), b"a");
        assert_eq!(encode(k(KeyCode::Char('A'), SHIFT), false).unwrap(), b"A");
        assert_eq!(encode(k(KeyCode::Char('é'), NONE), false).unwrap(), "é".as_bytes());
    }

    #[test]
    fn ctrl_letters_and_symbols() {
        assert_eq!(encode(k(KeyCode::Char('a'), CTRL), false).unwrap(), vec![0x01]);
        assert_eq!(encode(k(KeyCode::Char('c'), CTRL), false).unwrap(), vec![0x03]);
        assert_eq!(encode(k(KeyCode::Char('['), CTRL), false).unwrap(), vec![0x1b]); // Ctrl+[ = ESC
        assert_eq!(encode(k(KeyCode::Char(' '), CTRL), false).unwrap(), vec![0x00]); // Ctrl+Space = NUL
        // Ctrl+digit has no C0 byte → send the digit.
        assert_eq!(encode(k(KeyCode::Char('2'), CTRL), false).unwrap(), b"2");
    }

    #[test]
    fn alt_prefixes_esc() {
        assert_eq!(encode(k(KeyCode::Char('x'), ALT), false).unwrap(), vec![0x1b, b'x']);
        assert_eq!(encode(k(KeyCode::Enter, ALT), false).unwrap(), vec![0x1b, b'\r']);
    }

    #[test]
    fn named_keys() {
        assert_eq!(encode(k(KeyCode::Enter, NONE), false).unwrap(), b"\r");
        assert_eq!(encode(k(KeyCode::Tab, NONE), false).unwrap(), b"\t");
        assert_eq!(encode(k(KeyCode::BackTab, SHIFT), false).unwrap(), b"\x1b[Z");
        assert_eq!(encode(k(KeyCode::Esc, NONE), false).unwrap(), b"\x1b");
        assert_eq!(encode(k(KeyCode::Backspace, NONE), false).unwrap(), vec![0x7f]);
        assert_eq!(encode(k(KeyCode::Backspace, CTRL), false).unwrap(), vec![0x08]);
    }

    #[test]
    fn cursor_keys_honour_app_mode_and_mods() {
        assert_eq!(encode(k(KeyCode::Up, NONE), false).unwrap(), b"\x1b[A");
        assert_eq!(encode(k(KeyCode::Up, NONE), true).unwrap(), b"\x1bOA"); // DECCKM
        assert_eq!(encode(k(KeyCode::Left, NONE), false).unwrap(), b"\x1b[D");
        // Ctrl+Up -> CSI 1 ; 5 A   (mod = 1 + ctrl(4))
        assert_eq!(encode(k(KeyCode::Up, CTRL), false).unwrap(), b"\x1b[1;5A");
        // Shift+Up -> CSI 1 ; 2 A
        assert_eq!(encode(k(KeyCode::Up, SHIFT), true).unwrap(), b"\x1b[1;2A");
        // Home/End
        assert_eq!(encode(k(KeyCode::Home, NONE), false).unwrap(), b"\x1b[H");
        assert_eq!(encode(k(KeyCode::End, NONE), true).unwrap(), b"\x1bOF");
    }

    #[test]
    fn prefix_parsing() {
        assert_eq!(parse_prefix("C-a"), (KeyCode::Char('a'), CTRL));
        assert_eq!(parse_prefix("c-b"), (KeyCode::Char('b'), CTRL));
        assert_eq!(parse_prefix("M-x"), (KeyCode::Char('x'), ALT));
        assert_eq!(parse_prefix("a"), (KeyCode::Char('a'), CTRL)); // bare -> ctrl
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(encode(k(KeyCode::PageUp, NONE), false).unwrap(), b"\x1b[5~");
        assert_eq!(encode(k(KeyCode::PageDown, NONE), false).unwrap(), b"\x1b[6~");
        assert_eq!(encode(k(KeyCode::Delete, NONE), false).unwrap(), b"\x1b[3~");
        assert_eq!(encode(k(KeyCode::Delete, CTRL), false).unwrap(), b"\x1b[3;5~");
        assert_eq!(encode(k(KeyCode::F(1), NONE), false).unwrap(), b"\x1bOP");
        assert_eq!(encode(k(KeyCode::F(5), NONE), false).unwrap(), b"\x1b[15~");
        assert_eq!(encode(k(KeyCode::F(12), NONE), false).unwrap(), b"\x1b[24~");
        assert_eq!(encode(k(KeyCode::F(1), CTRL), false).unwrap(), b"\x1b[1;5P");
    }
}
