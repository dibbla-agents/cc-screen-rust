//! A single attached session: an `alacritty_terminal` emulator fed by the
//! WebSocket byte stream, rendered straight into a ratatui buffer. alacritty
//! gives a real multi-thousand-line scrollback grid (unlike vt100, whose view
//! was capped at one screen), so the wheel scrolls back through full history.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{ClipboardType, Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Processor};
use cc_screen_protocol::SNAPSHOT_RESET;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    Connecting,
    Open,
    Closed,
}

/// A message bound for the session's WS task.
pub enum WsOut {
    Input(Vec<u8>),
    Resize(u16, u16),
}

/// A terminal size that satisfies alacritty's `Dimensions` (history lives in the
/// `Config`, so `total_lines == screen_lines` here).
struct TermSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermSize {
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

/// Largest clipboard write we will forward, mirroring the web client's cap
/// (cc-screen-saas proposal 0077 A8). Far above any real `/copy`, far below
/// anything that would make the user's terminal emulator unhappy.
const CLIP_CAP: usize = 64 * 1024;

/// Captures the clipboard writes a *session* performs — the OSC 52 sequences
/// Claude Code and friends emit on every copy (proposal 0077 Part C).
///
/// Until now these landed in a `VoidListener`: alacritty decoded the sequence,
/// checked the `Osc52::OnlyCopy` gate, emitted `Event::ClipboardStore` — and the
/// event went nowhere, so every copy performed inside a session was silently
/// lost. The app drains this between frames and re-emits the sequence on the
/// TUI's own stdout, letting the user's terminal emulator perform the write.
///
/// Two rules live here rather than at the emission site, because this is where
/// the type information still exists:
///
///   - **`Selection` writes are dropped.** alacritty's `clipboard_store` maps
///     `c` to `Clipboard` but conflates `p` (primary) and `s` into `Selection`,
///     so forwarding those would PROMOTE a primary-selection write to the
///     user's system clipboard — exactly what Part A refuses.
///   - **`ClipboardLoad` is ignored explicitly.** The `Osc52::OnlyCopy` config
///     already denies the read/query form upstream, but the denied branch there
///     literally contains the code to format a reply, so the invariant is
///     pinned here rather than assumed.
#[derive(Clone, Default)]
pub struct ClipListener {
    pending: Arc<Mutex<Vec<String>>>,
}

impl ClipListener {
    /// Take everything captured since the last call.
    pub fn drain(&self) -> Vec<String> {
        match self.pending.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(_) => Vec::new(),
        }
    }
}

/// Normalise clipboard text arriving from a session, the same way the web
/// client does (proposal 0077 A2). The text is attacker-influenceable by
/// construction — the assistant runs with `--dangerously-skip-permissions`, so
/// a prompt injection anywhere in its input yields arbitrary bytes on the PTY —
/// and the auto-execute vector is a trailing newline or a bare CR pasted into a
/// shell, which no size cap addresses.
///
/// CRLF → LF, bare CR dropped, all other C0/C1 controls and DEL stripped
/// (`\t` and `\n` survive), bidi overrides/isolates stripped, trailing
/// whitespace-only lines stripped.
pub fn sanitize_clipboard(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                // CRLF collapses to LF; a bare CR is dropped outright.
                if chars.peek() == Some(&'\n') {
                    continue;
                }
            }
            '\t' | '\n' => out.push(c),
            '\u{061c}' | '\u{200e}' | '\u{200f}' => {}
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {}
            c if (c as u32) < 0x20 || c == '\u{7f}' || ((c as u32) >= 0x80 && (c as u32) <= 0x9f) => {}
            c => out.push(c),
        }
    }
    // Trailing whitespace-only lines, including the final newline itself.
    while out.ends_with(' ') || out.ends_with('\t') || out.ends_with('\n') {
        out.pop();
    }
    out
}

impl EventListener for ClipListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::ClipboardStore(ClipboardType::Clipboard, text) => {
                if text.is_empty() || text.len() > CLIP_CAP {
                    return; // empty = "clear the clipboard"; we never do that
                }
                let text = sanitize_clipboard(&text);
                if text.is_empty() {
                    return;
                }
                if let Ok(mut q) = self.pending.lock() {
                    // A queue, not a mailbox: bounded so a flood can't grow
                    // without limit between frames.
                    if q.len() < 8 {
                        q.push(text);
                    }
                }
            }
            // Selection-typed writes (OSC 52 `p`/`s`) and every read request
            // stay here. See the type doc.
            _ => {}
        }
    }
}

fn new_term(cols: u16, rows: u16, clip: ClipListener) -> Term<ClipListener> {
    let size = TermSize { cols: cols.max(1) as usize, rows: rows.max(1) as usize };
    let config = Config {
        // EXPLICIT, not inherited from Default: this is the setting that denies
        // the OSC 52 *read* form (`ESC]52;c;?`), which would otherwise turn any
        // program's stdout into a clipboard exfiltration channel. It is
        // load-bearing, so it is written down.
        osc52: Osc52::OnlyCopy,
        ..Config::default() // → 10000 lines history
    };
    Term::new(config, &size, clip)
}

pub struct Pane {
    /// Unique per-attach id — pane messages from a WS task carry it so the app
    /// routes bytes to the right box (and drops stragglers from a dropped box).
    pub id: u64,
    pub session: String,
    /// The machine the session lives on (empty for a single agent / hub-less).
    /// Part of the box's identity so the same session name on two machines stays
    /// distinct.
    pub machine: String,
    term: Term<ClipListener>,
    /// The sink the emulator hands captured OSC 52 writes to (0077 Part C).
    /// Held here so a rebuilt emulator keeps the same queue.
    clip: ClipListener,
    processor: Processor,
    cols: u16,
    rows: u16,
    conn: ConnState,
    out_tx: mpsc::Sender<WsOut>,
    task: JoinHandle<()>,
    /// The web-set session mark colour (proposal 0029) mapped to a terminal
    /// colour, kept in sync from the session list so the grid box border can use
    /// it as an accent. `None` = unmarked → today's default border. Display-only.
    accent: Option<Color>,
}

impl Pane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        session: String,
        machine: String,
        cols: u16,
        rows: u16,
        out_tx: mpsc::Sender<WsOut>,
        task: JoinHandle<()>,
    ) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let clip = ClipListener::default();
        Self {
            id,
            session,
            machine,
            term: new_term(cols, rows, clip.clone()),
            clip,
            processor: Processor::new(),
            cols,
            rows,
            conn: ConnState::Connecting,
            out_tx,
            task,
            accent: None,
        }
    }

    /// The web-set mark colour to accent this box's border with (proposal 0029),
    /// or `None` when unmarked. Refreshed from the session list, so a colour set
    /// on the phone shows here on the next poll. Display-only.
    pub fn accent(&self) -> Option<Color> {
        self.accent
    }

    /// Update the mark accent (from the latest session list). Display-only — the
    /// TUI never sets a colour, it only mirrors what the web stored.
    pub fn set_accent(&mut self, accent: Option<Color>) {
        self.accent = accent;
    }

    // (`Pane::title()` was removed in 0059 C1+C5 — the grid resolves a box's title
    // from the live session list at render time so a rename shows without
    // re-attaching; see `ui::grid::pane_title`.)

    /// Feed a chunk of PTY output into the emulator. A chunk that *starts* with
    /// the RIS reset is a fresh (re)attach snapshot / lagged-resync /
    /// clear-history payload — rebuild the emulator from scratch so the replayed
    /// history reconstructs cleanly with no stale state.
    pub fn process(&mut self, bytes: &[u8]) {
        let snapshot = bytes.starts_with(SNAPSHOT_RESET);
        if snapshot {
            self.term = new_term(self.cols, self.rows, self.clip.clone());
            self.processor = Processor::new();
        }
        self.processor.advance(&mut self.term, bytes);
        if snapshot {
            // A (re)attach snapshot replays the session's recent output
            // verbatim, so an OSC 52 emitted minutes ago would otherwise write
            // the user's clipboard "just now". Same rule as the web client's
            // attach quiet period (0077 A10).
            let _ = self.clip.drain();
        }
    }

    /// Take the clipboard writes this session performed since the last call
    /// (0077 Part C). Drained by the event loop between frames.
    pub fn take_clipboard(&mut self) -> Vec<String> {
        self.clip.drain()
    }

    pub fn set_state(&mut self, s: ConnState) {
        self.conn = s;
    }

    pub fn state(&self) -> ConnState {
        self.conn
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// DECCKM (application-cursor) mode, for input encoding.
    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    /// The child asked for mouse reporting (DECSET 1000/1002/1003) — it wants to
    /// own the wheel itself (Claude's fullscreen renderer, `htop`, `lazygit`).
    /// Proposal 0069 Part A: we forward wheel events instead of scrolling locally.
    pub fn mouse_mode(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// The child enabled SGR (1006) extended mouse encoding — pick `CSI < …M`
    /// over the legacy X10 `CSI M` form (which caps coordinates at 223).
    pub fn sgr_mouse(&self) -> bool {
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }

    /// The child is in the alternate screen (DECSET 1049). alacritty builds that
    /// grid with **zero** scrollback, so `scroll()` is pinned at offset 0 there —
    /// every scroll affordance has to route around it (0069 Parts B and D).
    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// xterm "alternate scroll" (DECSET 1007, default-on): in the alt screen a
    /// wheel step means arrow keys. Apps that own the wheel differently reset it.
    pub fn alternate_scroll(&self) -> bool {
        self.term.mode().contains(TermMode::ALTERNATE_SCROLL)
    }

    /// Resize the emulator and tell the server. No-op if unchanged.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(TermSize { cols: cols as usize, rows: rows as usize });
        let _ = self.out_tx.try_send(WsOut::Resize(cols, rows));
    }

    /// Scroll the view by `lines` (positive = back into history). alacritty
    /// clamps to `[0, history]`. Visual only — input still targets the session.
    pub fn scroll(&mut self, lines: isize) {
        self.term.scroll_display(Scroll::Delta(lines as i32));
    }

    /// Snap back to the live bottom.
    pub fn scroll_to_live(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Rows currently scrolled back (0 = live).
    pub fn scroll_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Send raw input bytes to the session.
    pub fn send_input(&self, bytes: Vec<u8>) {
        let _ = self.out_tx.try_send(WsOut::Input(bytes));
    }

    /// Paint the emulator's current view into `area`.
    fn render_into(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let content = self.term.renderable_content();
        // display_iter yields absolute grid lines; the visible row is
        // line + display_offset (top of the viewport is line == -offset).
        let offset = content.display_offset as i32;

        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            let col = indexed.point.column.0;
            if row < 0 || row >= area.height as i32 || col >= area.width as usize {
                continue;
            }
            let cell = indexed.cell;
            // The right half of a wide char: render a space (the glyph lives in
            // the preceding cell).
            let ch = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) { ' ' } else { cell.c };
            if let Some(bc) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) {
                let mut sbuf = [0u8; 4];
                bc.set_symbol(ch.encode_utf8(&mut sbuf));
                bc.set_style(style_of(cell));
            }
        }

        // Block cursor (reverse video) when visible and on-screen.
        if content.cursor.shape != CursorShape::Hidden {
            let row = content.cursor.point.line.0 + offset;
            let col = content.cursor.point.column.0;
            if row >= 0 && row < area.height as i32 && col < area.width as usize {
                if let Some(bc) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) {
                    bc.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}

impl Widget for &Pane {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.render_into(area, buf);
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Detaching: stop the WS task, which drops its socket and closes the
        // server-side attach (the session itself keeps running).
        self.task.abort();
    }
}

fn style_of(cell: &Cell) -> Style {
    let f = cell.flags;
    let mut m = Modifier::empty();
    if f.contains(Flags::BOLD) {
        m |= Modifier::BOLD;
    }
    if f.contains(Flags::ITALIC) {
        m |= Modifier::ITALIC;
    }
    if f.contains(Flags::UNDERLINE) {
        m |= Modifier::UNDERLINED;
    }
    if f.contains(Flags::DIM) {
        m |= Modifier::DIM;
    }
    if f.contains(Flags::INVERSE) {
        m |= Modifier::REVERSED;
    }
    if f.contains(Flags::HIDDEN) {
        m |= Modifier::HIDDEN;
    }
    if f.contains(Flags::STRIKEOUT) {
        m |= Modifier::CROSSED_OUT;
    }
    Style::default().fg(conv_color(cell.fg)).bg(conv_color(cell.bg)).add_modifier(m)
}

fn conv_color(c: AnsiColor) -> Color {
    match c {
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => Color::Indexed(i),
        AnsiColor::Named(n) => named_color(n),
    }
}

fn named_color(n: NamedColor) -> Color {
    use NamedColor as N;
    match n {
        N::Black => Color::Black,
        N::Red => Color::Red,
        N::Green => Color::Green,
        N::Yellow => Color::Yellow,
        N::Blue => Color::Blue,
        N::Magenta => Color::Magenta,
        N::Cyan => Color::Cyan,
        N::White => Color::Gray, // ANSI "white" is the dim white
        N::BrightBlack => Color::DarkGray,
        N::BrightRed => Color::LightRed,
        N::BrightGreen => Color::LightGreen,
        N::BrightYellow => Color::LightYellow,
        N::BrightBlue => Color::LightBlue,
        N::BrightMagenta => Color::LightMagenta,
        N::BrightCyan => Color::LightCyan,
        N::BrightWhite => Color::White,
        // Foreground/Background/Cursor/dim/bright-fg → terminal default.
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn pane(cols: u16, rows: u16) -> Pane {
        let (tx, _rx) = mpsc::channel(4);
        let task = tokio::spawn(async {});
        Pane::new(1, "s".into(), String::new(), cols, rows, tx, task)
    }

    fn render(p: &Pane, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| f.render_widget(p, f.area())).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[tokio::test]
    async fn scrollback_offset_moves_and_returns_live() {
        let mut p = pane(80, 5);
        for i in 0..50 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(p.scroll_offset(), 0); // live
        p.scroll(10);
        assert_eq!(p.scroll_offset(), 10);
        p.scroll(-3);
        assert_eq!(p.scroll_offset(), 7);
        p.scroll(-100); // clamps at live
        assert_eq!(p.scroll_offset(), 0);
        p.scroll(5);
        p.scroll_to_live();
        assert_eq!(p.scroll_offset(), 0);
    }

    #[tokio::test]
    async fn scrolls_back_multiple_screens() {
        // The whole point of the swap: scroll back FAR more than one screen.
        let mut p = pane(20, 5);
        for i in 0..40 {
            p.process(format!("LINE_{i}\r\n").as_bytes());
        }
        let live = render(&p, 20, 5);
        assert!(live.contains("LINE_39"), "live shows newest: {live:?}");

        p.scroll(20); // 20 lines back — 4 screens
        assert_eq!(p.scroll_offset(), 20);
        let scrolled = render(&p, 20, 5);
        assert!(!scrolled.contains("LINE_39"), "newest hidden: {scrolled:?}");
        assert!(scrolled.contains("LINE_18"), "deep history shown: {scrolled:?}");
    }

    #[tokio::test]
    async fn huge_delta_lands_at_top_of_history() {
        // `g` (top) in keyboard-scroll mode passes a delta far larger than the
        // history; alacritty clamps it to the oldest available line rather than
        // overshooting, and the view then shows the very first line.
        let mut p = pane(20, 5);
        for i in 0..40 {
            p.process(format!("LINE_{i}\r\n").as_bytes());
        }
        p.scroll(1_000_000); // the SCROLL_TOP delta app.rs sends for `g`
        let top = p.scroll_offset();
        assert!(top > 0, "a huge delta scrolls back into history: {top}");
        assert_eq!(top, p.scroll_offset(), "offset is stable once clamped at the top");
        let view = render(&p, 20, 5);
        assert!(view.contains("LINE_0"), "the top view shows the oldest line: {view:?}");
    }

    /// 0069: the alternate screen has no history by construction, so the wheel's
    /// local path is a permanent no-op there — which is exactly why `handle_mouse`
    /// has to route around it. The mode flags it routes on are exposed here.
    #[tokio::test]
    async fn alt_screen_pins_scroll_and_exposes_modes() {
        let mut p = pane(40, 5);
        for i in 0..40 {
            p.process(format!("L{i}\r\n").as_bytes());
        }
        assert!(!p.alt_screen() && !p.mouse_mode());
        assert!(p.alternate_scroll(), "DECSET 1007 is default-on");
        p.scroll(10);
        assert_eq!(p.scroll_offset(), 10, "primary screen scrolls back");

        // The child goes fullscreen and grabs the mouse (Claude ≥ 2.1.89).
        p.process(b"\x1b[?1049h\x1b[?1002h\x1b[?1006h");
        assert!(p.alt_screen() && p.mouse_mode() && p.sgr_mouse());
        assert_eq!(p.scroll_offset(), 0, "entering the alt screen lands live");
        p.scroll(10);
        assert_eq!(p.scroll_offset(), 0, "the alt grid has zero history — pinned at 0");

        // …and back out: the primary grid (and its history) is intact.
        p.process(b"\x1b[?1002l\x1b[?1006l\x1b[?1049l");
        assert!(!p.alt_screen() && !p.mouse_mode() && !p.sgr_mouse());
        p.scroll_to_live();
        p.scroll(10);
        assert_eq!(p.scroll_offset(), 10, "primary history survived the round trip");
    }

    #[tokio::test]
    async fn output_while_scrolled_does_not_panic() {
        let mut p = pane(40, 5);
        for i in 0..40 {
            p.process(format!("L{i}\r\n").as_bytes());
        }
        p.scroll(15);
        for i in 40..120 {
            p.process(format!("L{i}\r\n").as_bytes()); // heavy output while scrolled
        }
        let _ = render(&p, 40, 5); // must not panic
        p.scroll_to_live();
        assert_eq!(p.scroll_offset(), 0);
    }

    // ── OSC 52 clipboard forwarding (proposal 0077 Part C) ───────────────────

    fn osc52(text: &str) -> Vec<u8> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!("\x1b]52;c;{b64}\x07").into_bytes()
    }

    #[tokio::test]
    async fn captures_a_clipboard_write_from_the_session() {
        let mut p = pane(20, 4);
        p.process(&osc52("hello from claude"));
        assert_eq!(p.take_clipboard(), vec!["hello from claude".to_string()]);
        // Draining is destructive: the same copy is not delivered twice.
        assert!(p.take_clipboard().is_empty());
    }

    #[tokio::test]
    async fn the_sequence_leaves_no_mark_on_the_frame() {
        let mut p = pane(10, 2);
        p.process(b"hi");
        let before = render(&p, 10, 2);
        p.process(&osc52("clipboard payload"));
        assert_eq!(render(&p, 10, 2), before, "OSC 52 must not paint anything");
    }

    #[tokio::test]
    async fn the_query_form_is_denied_and_writes_nothing_back() {
        // Osc52::OnlyCopy denies `clipboard_load` upstream, so a `?` payload
        // produces no re-emission AND no PTY write. The TUI is structurally
        // immune to the read form; this pins it (0077 Part C).
        let mut p = pane(20, 4);
        p.process(b"\x1b]52;c;?\x07");
        assert!(p.take_clipboard().is_empty());
    }

    #[tokio::test]
    async fn a_selection_typed_write_is_dropped_not_promoted() {
        // alacritty conflates OSC 52 `p` and `s` into ClipboardType::Selection.
        // Forwarding one would promote a primary-selection write to the user's
        // system clipboard, which Part A refuses.
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"primary only");
        let mut p = pane(20, 4);
        p.process(format!("\x1b]52;p;{b64}\x07").as_bytes());
        assert!(p.take_clipboard().is_empty());
    }

    #[tokio::test]
    async fn a_replayed_snapshot_does_not_write_the_clipboard() {
        // A (re)attach snapshot replays recent output verbatim; an OSC 52 from
        // minutes ago must not land on the clipboard "just now" (0077 A10).
        let mut p = pane(20, 4);
        let mut chunk = SNAPSHOT_RESET.to_vec();
        chunk.extend_from_slice(&osc52("stale copy"));
        p.process(&chunk);
        assert!(p.take_clipboard().is_empty());
    }

    #[tokio::test]
    async fn an_oversize_write_is_ignored() {
        let mut p = pane(20, 4);
        p.process(&osc52(&"x".repeat(CLIP_CAP + 1)));
        assert!(p.take_clipboard().is_empty());
    }

    #[test]
    fn sanitize_strips_the_attacker_shaped_vectors() {
        // The same A2 vectors the web client's vitest pins, so the two clients
        // cannot drift on what a session is allowed to put on a clipboard.
        assert_eq!(sanitize_clipboard("rm -rf /\n"), "rm -rf /");
        assert_eq!(sanitize_clipboard("echo hi\rrm -rf /"), "echo hirm -rf /");
        assert_eq!(sanitize_clipboard("a\r\nb"), "a\nb");
        assert_eq!(sanitize_clipboard("a\u{1}b\u{7f}c\u{9b}d"), "abcd");
        assert_eq!(sanitize_clipboard("safe\u{202e}txt.exe"), "safetxt.exe");
        assert_eq!(sanitize_clipboard("payload\n\n   \n"), "payload");
        assert_eq!(sanitize_clipboard("a\tb\nc"), "a\tb\nc");
    }

    #[tokio::test]
    async fn a_captured_write_is_sanitised_before_it_is_queued() {
        let mut p = pane(20, 4);
        p.process(&osc52("rm -rf /\n"));
        assert_eq!(p.take_clipboard(), vec!["rm -rf /".to_string()]);
    }

    #[test]
    fn the_re_emitted_sequence_is_re_encoded_from_the_decoded_text() {
        let mut out = Vec::new();
        crate::term::write_osc52(&mut out, "hello").unwrap();
        assert_eq!(out, b"\x1b]52;c;aGVsbG8=\x07");
    }

}
