//! A thin `ratatui` backend that re-anchors the cursor after any glyph whose
//! on-screen width the host terminal might disagree about.
//!
//! ## Why this exists
//!
//! `ccs` paints an `alacritty_terminal` grid into ratatui, which renders
//! *incrementally* onto the user's real terminal: each frame it diffs against
//! the previous one and writes only the changed cells, relying on the host's
//! cursor auto-advancing **exactly** as ratatui's width model (`unicode-width`)
//! predicts. For ASCII and unambiguous-width text that always holds.
//!
//! It breaks for glyphs that some terminals (WezTerm, Kitty, …) render two
//! cells wide while `unicode-width` calls them one: East-Asian **ambiguous**
//! symbols (`✓ ● ▶`, arrows) and **private-use** Nerd-Font / powerline glyphs.
//! Each such glyph shifts the host cursor +1 past where ratatui thinks it is, so
//! ratatui's next diff stops rewriting cells it believes are unchanged — leaving
//! orphaned characters that pile up as live output streams (clean on attach,
//! worse the faster the screen updates). The browser client doesn't hit this:
//! xterm.js paints each cell into a fixed canvas grid and never delegates glyph
//! advance to a host terminal.
//!
//! ## The fix
//!
//! Split each `draw` into runs that break right after an *untrusted* cell, so
//! the next cell is emitted with a fresh absolute `MoveTo`. A glyph the host
//! renders too wide can then shift the cursor by at most one cell before we
//! re-anchor — drift can't accumulate. A cell is **untrusted** when it's a
//! genuinely-narrow (`unicode-width == 1`) glyph from a block terminals commonly
//! upgrade to a 2-cell "emoji"/icon: the private-use planes (Nerd Fonts,
//! powerline) and the symbol/dingbat/geometric/arrow blocks (`✓ ● ▶ ★ ⚡`). That
//! keeps the hot paths cheap — ASCII, box-drawing, block, braille and
//! genuinely-wide (`width == 2`, CJK/emoji, which the host also renders wide)
//! runs stay batched exactly as before, so fast-scrolling output and agent TUIs
//! pay nothing; only the handful of icon glyphs in a prompt cost a cursor move.

use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use unicode_width::UnicodeWidthChar;

/// `CrosstermBackend` plus the run-splitting `draw` above.
pub struct AnchoredBackend<W: Write> {
    inner: CrosstermBackend<W>,
}

impl<W: Write> AnchoredBackend<W> {
    pub fn new(writer: W) -> Self {
        Self { inner: CrosstermBackend::new(writer) }
    }
}

/// Narrow (`unicode-width == 1`) glyphs that some terminals nonetheless render
/// two cells wide, sizing them to the font's advance: the private-use planes
/// (Nerd Fonts, powerline) and the symbol/dingbat/geometric/arrow blocks. These
/// are the cells whose width the host may disagree with, so we re-anchor after
/// them. Box-drawing (`2500–257F`), block (`2580–259F`) and braille (`2800–28FF`)
/// are deliberately excluded — they're narrow on every terminal.
fn host_may_widen(c: char) -> bool {
    matches!(c as u32,
        0x2190..=0x21FF |   // arrows
        0x2300..=0x23FF |   // misc technical (UI / power / keyboard symbols)
        0x25A0..=0x25FF |   // geometric shapes (● ○ ▶ ◀ ◆ ■ ▲ …)
        0x2600..=0x27BF |   // misc symbols + dingbats (★ ⚡ ☢ ✓ ✗ …)
        0x2900..=0x29FF |   // supplemental arrows-B / misc math symbols
        0x2B00..=0x2BFF |   // misc symbols and arrows (⭐ ⬆ …)
        0xE000..=0xF8FF |   // BMP private use (Nerd Fonts, powerline separators)
        0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD // supplementary private-use planes
    )
}

/// Whether a run may keep auto-advancing across this cell — i.e. the host is sure
/// to move the cursor exactly as many columns as ratatui assumes. Genuinely-wide
/// glyphs (`width == 2`: CJK, emoji) are trusted because the host renders them
/// wide too; narrow glyphs are trusted unless they're in a block the host might
/// upgrade (see `host_may_widen`).
fn trusted_advance(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let c = match (chars.next(), chars.next()) {
        (Some(c), None) => c,
        // Empty (a skipped wide-char slot) or a multi-codepoint cluster: don't
        // trust the host to advance as we expect — re-anchor after it.
        _ => return false,
    };
    match c.width() {
        Some(2) => true,            // wide everywhere — host agrees
        Some(1) => !host_may_widen(c),
        _ => false,                 // zero-width / control — re-anchor defensively
    }
}

impl<W: Write> Backend for AnchoredBackend<W> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        // Materialize so we can cut the stream into runs. A run ends when the
        // next cell isn't physically adjacent (ratatui already skips wide-char
        // slots, which shows up as a column gap) OR the previous cell was an
        // untrusted glyph. Each `inner.draw` re-emits a leading absolute MoveTo,
        // so a mis-widthed glyph can't push everything after it out of place.
        let cells: Vec<(u16, u16, &Cell)> = content.collect();
        let mut start = 0;
        for i in 1..cells.len() {
            let (x, y, _) = cells[i];
            let (px, py, prev) = cells[i - 1];
            let adjacent = y == py && x == px + 1;
            if !adjacent || !trusted_advance(prev.symbol()) {
                self.inner.draw(cells[start..i].iter().copied())?;
                start = i;
            }
        }
        if start < cells.len() {
            self.inner.draw(cells[start..].iter().copied())?;
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend;
    use ratatui::buffer::Cell;

    // Count cursor-position (CUP, `ESC [ row ; col H`) sequences in the output —
    // one per run, since each `inner.draw` re-anchors with a leading MoveTo.
    fn moves(symbols: &[&str]) -> usize {
        let cells: Vec<Cell> = symbols
            .iter()
            .map(|s| {
                let mut c = Cell::default();
                c.set_symbol(s);
                c
            })
            .collect();
        let content: Vec<(u16, u16, &Cell)> =
            cells.iter().enumerate().map(|(i, c)| (i as u16, 0u16, c)).collect();
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut backend = AnchoredBackend::new(&mut buf);
            backend.draw(content.into_iter()).unwrap();
        }
        buf.iter().filter(|&&b| b == b'H').count()
    }

    #[test]
    fn ascii_run_is_a_single_anchored_write() {
        // Trusted, adjacent cells stay one batched run → one MoveTo.
        assert_eq!(moves(&["a", "b", "c"]), 1);
    }

    #[test]
    fn box_drawing_and_cjk_stay_batched() {
        // Box-drawing (width 1, narrow everywhere) and CJK (width 2, host renders
        // wide too) must NOT force re-anchors — keeps agent TUIs / fast output
        // cheap. All trusted + adjacent → one batched run → one MoveTo.
        assert!(trusted_advance("│") && trusted_advance("─") && trusted_advance("中"));
        assert_eq!(moves(&["│", "─", "╭", "┤"]), 1);
    }

    #[test]
    fn ambiguous_glyph_forces_a_reanchor_after_it() {
        // `✓` is East-Asian-ambiguous (width 1, width_cjk 2): the host may render
        // it 2 wide, so the cell after it must be re-anchored → an extra MoveTo.
        assert!(!trusted_advance("✓"));
        assert_eq!(moves(&["a", "✓", "b"]), 2);
    }

    #[test]
    fn private_use_glyph_forces_a_reanchor_after_it() {
        // Powerline separator (U+E0B0) — PUA, sized to the font's advance.
        assert!(!trusted_advance("\u{e0b0}"));
        assert_eq!(moves(&["a", "\u{e0b0}", "b"]), 2);
    }
}
