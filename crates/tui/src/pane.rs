//! A single attached session: an `alacritty_terminal` emulator fed by the
//! WebSocket byte stream, rendered straight into a ratatui buffer. alacritty
//! gives a real multi-thousand-line scrollback grid (unlike vt100, whose view
//! was capped at one screen), so the wheel scrolls back through full history.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
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

fn new_term(cols: u16, rows: u16) -> Term<VoidListener> {
    let size = TermSize { cols: cols.max(1) as usize, rows: rows.max(1) as usize };
    Term::new(Config::default(), &size, VoidListener) // Config::default → 10000 lines history
}

pub struct Pane {
    /// Unique per-attach id — pane messages from a WS task carry it so the app
    /// routes bytes to the right box (and drops stragglers from a dropped box).
    pub id: u64,
    pub session: String,
    term: Term<VoidListener>,
    processor: Processor,
    cols: u16,
    rows: u16,
    conn: ConnState,
    out_tx: mpsc::Sender<WsOut>,
    task: JoinHandle<()>,
}

impl Pane {
    pub fn new(
        id: u64,
        session: String,
        cols: u16,
        rows: u16,
        out_tx: mpsc::Sender<WsOut>,
        task: JoinHandle<()>,
    ) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            id,
            session,
            term: new_term(cols, rows),
            processor: Processor::new(),
            cols,
            rows,
            conn: ConnState::Connecting,
            out_tx,
            task,
        }
    }

    /// Feed a chunk of PTY output into the emulator. A chunk that *starts* with
    /// the RIS reset is a fresh (re)attach snapshot / lagged-resync /
    /// clear-history payload — rebuild the emulator from scratch so the replayed
    /// history reconstructs cleanly with no stale state.
    pub fn process(&mut self, bytes: &[u8]) {
        if bytes.starts_with(SNAPSHOT_RESET) {
            self.term = new_term(self.cols, self.rows);
            self.processor = Processor::new();
        }
        self.processor.advance(&mut self.term, bytes);
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
        Pane::new(1, "s".into(), cols, rows, tx, task)
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
}
