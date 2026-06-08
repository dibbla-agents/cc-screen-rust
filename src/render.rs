//! Server-side terminal snapshot.
//!
//! The session pump feeds the raw PTY byte stream into a real terminal emulator
//! (alacritty_terminal), so the server always holds the *interpreted* grid +
//! scrollback. On (re)attach we serialize that grid back into a **size-agnostic**
//! repaint — RIS, the live terminal modes, then one styled text line per grid
//! row — instead of replaying the raw byte history.
//!
//! Why: the agents (Claude/codex/…) render with absolute cursor-column moves and
//! fixed-line-count screen erases, all computed for the PTY size. Replaying that
//! raw history into a client grid of any *other* size mis-lays it — columns
//! pending-wrap into a per-word "staircase", and oversized erase/scroll loops
//! shove stale frames into scrollback as duplicate UIs. A clean repaint carries
//! no absolute positions or erases, so every client renders it correctly at its
//! own size; the live stream that follows is fine because min-size pins the PTY
//! to the narrowest client (every client is ≥ PTY width, so positions fit).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use cc_screen_protocol::SNAPSHOT_RESET;

/// Scrollback retained per session (lines) — matches the previous vt100 setting.
const HISTORY: usize = 5000;

/// Style flags that make an otherwise-space cell visible (so it isn't trimmed as
/// trailing blank). WIDE_CHAR_SPACER / WRAPLINE etc. are layout, not ink.
const INK_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

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

/// A server-side terminal emulator fed by one session's PTY output.
pub struct Emulator {
    term: Term<VoidListener>,
    parser: Processor,
    cols: u16,
    rows: u16,
}

impl Emulator {
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let cfg = Config { scrolling_history: HISTORY, ..Default::default() };
        let term = Term::new(cfg, &Size { cols: cols as usize, rows: rows as usize }, VoidListener);
        Self { term, parser: Processor::new(), cols, rows }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Size { cols: cols as usize, rows: rows as usize });
    }

    /// Drop scrollback, keep the visible screen (the clear-history feature).
    pub fn clear_history(&mut self) {
        self.process(b"\x1b[3J");
    }

    /// Last non-blank line of the visible screen — the session-list preview.
    pub fn preview(&self) -> String {
        let grid = self.term.grid();
        let cols = grid.columns();
        for li in (0..grid.screen_lines() as i32).rev() {
            let row = &grid[Line(li)];
            let s: String = (0..cols).map(|c| row[Column(c)].c).collect();
            let t = s.trim();
            if !t.is_empty() {
                return t.chars().take(120).collect();
            }
        }
        String::new()
    }

    /// Plain-text render of the last `max_lines` non-blank rows of the buffer
    /// (scrollback + screen), one row per line, no ANSI/SGR. This is the LLM
    /// context window for the session summary (proposal 0022): `snapshot()` is
    /// wrong for that (it carries SGR codes), `preview()` is only one line.
    /// Leading/trailing blank rows are dropped so a short session returns just its
    /// content; the output is stable for an unchanged grid (drives the hash gate).
    pub fn tail_text(&self, max_lines: usize) -> String {
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut lines: Vec<String> = Vec::new();
        for li in grid.topmost_line().0..=grid.bottommost_line().0 {
            let row = &grid[Line(li)];
            let s: String = (0..cols).map(|c| row[Column(c)].c).collect();
            lines.push(s.trim_end().to_string());
        }
        // Trim blank rows at both ends (no padding for short sessions).
        while lines.first().is_some_and(|s| s.is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|s| s.is_empty()) {
            lines.pop();
        }
        if lines.len() > max_lines {
            lines = lines.split_off(lines.len() - max_lines);
        }
        lines.join("\n")
    }

    /// A clean, size-agnostic repaint of scrollback + screen, prefixed with RIS.
    pub fn snapshot(&self) -> Vec<u8> {
        let grid = self.term.grid();
        let cols = grid.columns();
        let mode = *self.term.mode();
        let mut out: Vec<u8> = Vec::with_capacity(8 * 1024);
        out.extend_from_slice(SNAPSHOT_RESET); // RIS — back to default modes/screen
        emit_modes(&mut out, mode);

        // Emit every grid line oldest-first, each on its own physical line. The
        // client lays them out at its own width (≥ PTY width via min-size, so no
        // rewrap), and the history lines scroll up into its scrollback.
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        for (i, li) in (top..=bottom).enumerate() {
            if i != 0 {
                out.extend_from_slice(b"\r\n");
            }
            emit_row(&mut out, grid, Line(li), cols);
        }

        // Park the cursor where the agent has it (screen-relative; the agent's
        // next redraw re-anchors at home anyway).
        let cur = self.term.renderable_content().cursor;
        let row = (cur.point.line.0.max(0) as usize).min(self.rows.saturating_sub(1) as usize);
        let col = (cur.point.column.0).min(cols.saturating_sub(1));
        out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
        if !mode.contains(TermMode::SHOW_CURSOR) {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }
}

/// Re-assert the modes the agent has set that differ from a freshly-reset
/// terminal's defaults and that affect input/behaviour — so arrow keys (DECCKM),
/// paste (bracketed paste) and mouse reporting keep working after a reattach
/// repaint. RIS cleared them all, so we only ever turn things *on* (plus the one
/// default-on mode, autowrap, that we turn off if the agent did).
fn emit_modes(out: &mut Vec<u8>, mode: TermMode) {
    if mode.contains(TermMode::ALT_SCREEN) {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    if mode.contains(TermMode::APP_CURSOR) {
        out.extend_from_slice(b"\x1b[?1h");
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.extend_from_slice(b"\x1b=");
    }
    if mode.contains(TermMode::BRACKETED_PASTE) {
        out.extend_from_slice(b"\x1b[?2004h");
    }
    if mode.contains(TermMode::FOCUS_IN_OUT) {
        out.extend_from_slice(b"\x1b[?1004h");
    }
    if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        out.extend_from_slice(b"\x1b[?1000h");
    }
    if mode.contains(TermMode::MOUSE_DRAG) {
        out.extend_from_slice(b"\x1b[?1002h");
    }
    if mode.contains(TermMode::MOUSE_MOTION) {
        out.extend_from_slice(b"\x1b[?1003h");
    }
    if mode.contains(TermMode::UTF8_MOUSE) {
        out.extend_from_slice(b"\x1b[?1005h");
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        out.extend_from_slice(b"\x1b[?1006h");
    }
    if mode.contains(TermMode::ALTERNATE_SCROLL) {
        out.extend_from_slice(b"\x1b[?1007h");
    }
    if !mode.contains(TermMode::LINE_WRAP) {
        out.extend_from_slice(b"\x1b[?7l");
    }
}

/// Serialize one grid row as styled text, trimming trailing blank cells and
/// resetting SGR at the end so the following `\r\n` starts clean.
fn emit_row(out: &mut Vec<u8>, grid: &Grid<Cell>, line: Line, cols: usize) {
    let row = &grid[line];
    // Last column carrying ink — everything past it is blank padding we drop.
    let mut end = 0usize;
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        if !is_blank(cell) {
            end = c + 1;
        }
    }

    let mut sgr: Vec<u8> = b"\x1b[0m".to_vec(); // client is at default after RIS / prior \r\n
    for c in 0..end {
        let cell = &row[Column(c)];
        // The trailing half of a wide glyph occupies a cell with no char of its
        // own; the glyph itself sits in the preceding cell, so skip the spacer.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let want = cell_sgr(cell);
        if want != sgr {
            out.extend_from_slice(&want);
            sgr = want;
        }
        let mut b = [0u8; 4];
        out.extend_from_slice(cell.c.encode_utf8(&mut b).as_bytes());
    }
    if sgr != b"\x1b[0m" {
        out.extend_from_slice(b"\x1b[0m");
    }
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && matches!(cell.fg, Color::Named(NamedColor::Foreground))
        && matches!(cell.bg, Color::Named(NamedColor::Background))
        && !cell.flags.intersects(INK_FLAGS)
}

/// Full self-contained SGR for a cell's style: `ESC [ 0 ; … m` (reset first, then
/// the attributes), so two cells with the same style produce identical bytes and
/// we can emit it only on change.
fn cell_sgr(cell: &Cell) -> Vec<u8> {
    let mut p = b"\x1b[0".to_vec();
    let f = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, b"1".as_slice()),
        (Flags::DIM, b"2"),
        (Flags::ITALIC, b"3"),
        (Flags::UNDERLINE, b"4"),
        (Flags::INVERSE, b"7"),
        (Flags::HIDDEN, b"8"),
        (Flags::STRIKEOUT, b"9"),
    ] {
        if f.contains(flag) {
            p.push(b';');
            p.extend_from_slice(code);
        }
    }
    push_color(&mut p, cell.fg, true);
    push_color(&mut p, cell.bg, false);
    p.push(b'm');
    p
}

fn push_color(p: &mut Vec<u8>, color: Color, fg: bool) {
    let (ext, base256) = if fg { (b"38".as_slice(), 38) } else { (b"48".as_slice(), 48) };
    let _ = base256;
    match color {
        Color::Spec(rgb) => {
            p.push(b';');
            p.extend_from_slice(ext);
            p.extend_from_slice(format!(";2;{};{};{}", rgb.r, rgb.g, rgb.b).as_bytes());
        }
        Color::Indexed(i) => {
            p.push(b';');
            p.extend_from_slice(ext);
            p.extend_from_slice(format!(";5;{i}").as_bytes());
        }
        Color::Named(n) => {
            if let Some(code) = named_code(n, fg) {
                p.extend_from_slice(format!(";{code}").as_bytes());
            }
            // Foreground / Background / Cursor / dim-special → terminal default,
            // already covered by the leading `0` reset, so emit nothing.
        }
    }
}

/// ANSI SGR number for one of the 16 named colours, or `None` for default-ish
/// names (handled by the reset). `fg` selects the 30/90 vs 40/100 plane.
fn named_code(n: NamedColor, fg: bool) -> Option<u16> {
    use NamedColor as N;
    let (idx, bright) = match n {
        N::Black => (0, false),
        N::Red => (1, false),
        N::Green => (2, false),
        N::Yellow => (3, false),
        N::Blue => (4, false),
        N::Magenta => (5, false),
        N::Cyan => (6, false),
        N::White => (7, false),
        N::BrightBlack => (0, true),
        N::BrightRed => (1, true),
        N::BrightGreen => (2, true),
        N::BrightYellow => (3, true),
        N::BrightBlue => (4, true),
        N::BrightMagenta => (5, true),
        N::BrightCyan => (6, true),
        N::BrightWhite => (7, true),
        _ => return None,
    };
    let base = match (fg, bright) {
        (true, false) => 30,
        (true, true) => 90,
        (false, false) => 40,
        (false, true) => 100,
    };
    Some(base + idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full buffer text (scrollback + screen), one row per line, trailing-trimmed.
    fn full_text(e: &Emulator) -> String {
        let grid = e.term.grid();
        let cols = grid.columns();
        (grid.topmost_line().0..=grid.bottommost_line().0)
            .map(|li| {
                let row = &grid[Line(li)];
                (0..cols).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Produce a snapshot at the source size, then replay it into a fresh
    /// emulator at the destination size and read its full buffer back — the exact
    /// path a client takes on (re)attach. min-size guarantees dst >= src in
    /// practice (the PTY is pinned to the narrowest client), so callers test
    /// same-or-wider.
    fn round_trip_through(src_cols: u16, src_rows: u16, bytes: &[u8], dst_cols: u16, dst_rows: u16) -> String {
        let mut src = Emulator::new(src_cols, src_rows);
        src.process(bytes);
        let snap = src.snapshot();
        let mut dst = Emulator::new(dst_cols, dst_rows);
        dst.process(&snap);
        full_text(&dst)
    }

    #[test]
    fn snapshot_starts_with_ris() {
        let mut e = Emulator::new(80, 24);
        e.process(b"hello");
        assert!(e.snapshot().starts_with(SNAPSHOT_RESET));
    }

    #[test]
    fn plain_text_survives_reattach_and_widening() {
        // Reattach at the same size (the common case) and at a wider size (a
        // second, larger client under min-size) — text must stay contiguous.
        let txt = "the quick brown fox jumps over the lazy dog";
        for (c, r) in [(80, 24), (100, 30)] {
            let out = round_trip_through(80, 24, txt.as_bytes(), c, r);
            assert!(out.contains(txt), "text should survive {c}x{r} replay, got:\n{out}");
        }
    }

    #[test]
    fn absolute_positioned_words_survive_replay() {
        // Mimic the agent: place words with absolute column moves (CHA) — the
        // exact thing that staircased on raw replay. After a clean repaint they
        // must be contiguous at the same and a wider width.
        let bytes = b"\x1b[3Gthe\x1b[7Gold\x1b[11Glighthouse";
        for (c, r) in [(80, 24), (120, 40)] {
            let out = round_trip_through(80, 24, bytes, c, r);
            assert!(out.contains("the old lighthouse"), "got at {c}x{r}:\n{out}");
        }
    }

    #[test]
    fn full_screen_redraw_does_not_duplicate_on_smaller_replay() {
        // A frame built for a TALL terminal (erase N lines + reprint), replayed
        // on a SHORT one, must not leave a duplicate copy in scrollback. We draw a
        // unique footer once; after snapshot+replay it must appear exactly once in
        // the visible screen + scrollback.
        let mut tall = Emulator::new(40, 50);
        // Home, clear 50 lines, repaint a couple lines incl. a marker footer.
        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1b[H");
        for _ in 0..50 {
            frame.extend_from_slice(b"\x1b[2K\x1b[1B");
        }
        frame.extend_from_slice(b"\x1b[H");
        frame.extend_from_slice(b"top line\r\n");
        frame.extend_from_slice(b"UNIQUE_FOOTER_MARK");
        tall.process(&frame);
        let snap = tall.snapshot();

        let mut short = Emulator::new(40, 12);
        short.process(&snap);
        // Count the marker across the whole buffer (history + screen).
        let grid = short.term.grid();
        let cols = grid.columns();
        let mut full = String::new();
        for li in grid.topmost_line().0..=grid.bottommost_line().0 {
            let row = &grid[Line(li)];
            full.push_str(&(0..cols).map(|c| row[Column(c)].c).collect::<String>());
            full.push('\n');
        }
        let n = full.matches("UNIQUE_FOOTER_MARK").count();
        assert_eq!(n, 1, "footer must appear exactly once, found {n}:\n{full}");
    }

    #[test]
    fn tail_text_is_plain_bounded_and_stable() {
        let mut e = Emulator::new(80, 24);
        // Styled output: tail_text must drop the SGR bytes.
        e.process(b"\x1b[1;31mERROR\x1b[0m: build failed\r\n");
        e.process(b"line two\r\nline three\r\n");
        let t = e.tail_text(200);
        assert!(!t.contains('\u{1b}'), "no ANSI escapes in tail_text: {t:?}");
        assert!(t.contains("ERROR: build failed"));
        assert!(t.contains("line three"));
        // Stable for an unchanged grid (the hash gate relies on this).
        assert_eq!(t, e.tail_text(200));
        // Bounded to the last N rows.
        let mut tall = Emulator::new(40, 10);
        for i in 0..50 {
            tall.process(format!("row{i}\r\n").as_bytes());
        }
        let bounded = tall.tail_text(5);
        assert_eq!(bounded.lines().count(), 5, "bounded to 5 rows: {bounded:?}");
        assert!(bounded.contains("row49"), "keeps the most recent rows");
        assert!(!bounded.contains("row10"), "drops older rows");
    }

    #[test]
    fn modes_are_reemitted() {
        let mut e = Emulator::new(80, 24);
        // Agent turns on app-cursor + bracketed paste + SGR mouse.
        e.process(b"\x1b[?1h\x1b[?2004h\x1b[?1006h");
        let snap = e.snapshot();
        let s = String::from_utf8_lossy(&snap);
        assert!(s.contains("\x1b[?1h"), "app-cursor not re-emitted");
        assert!(s.contains("\x1b[?2004h"), "bracketed paste not re-emitted");
        assert!(s.contains("\x1b[?1006h"), "sgr mouse not re-emitted");
    }
}
