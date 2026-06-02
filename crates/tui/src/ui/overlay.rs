//! Centered modal overlays for the switcher: a kill/exit confirm and the
//! new-session form.

use cc_screen_protocol::SessionInfo;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::ui::util::truncate;

const PANEL_BG: Color = Color::Rgb(20, 28, 38);

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn panel(f: &mut Frame, area: Rect, title: &str) -> Rect {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(PANEL_BG).fg(Color::Gray));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// A yes/no confirm dialog.
pub fn confirm(f: &mut Frame, title: &str, body: &str) {
    let w = (body.len() as u16 + 6).clamp(28, f.area().width.max(28));
    let area = centered(f.area(), w, 5);
    let inner = panel(f, area, title);
    let p = Paragraph::new(vec![
        Line::from(body),
        Line::from(""),
        Line::from(Span::styled(
            "y confirm    n / esc cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .alignment(Alignment::Center);
    f.render_widget(p, inner);
}

pub struct NewSessionView<'a> {
    pub tool: &'a str,
    pub name: &'a str,
    pub dir: &'a str,
    pub field: usize, // 0 = name, 1 = dir
    pub error: Option<&'a str>,
}

/// The new-session form: tool selector + name + dir, with the focused field
/// marked and a trailing cursor block.
pub fn new_session(f: &mut Frame, v: &NewSessionView) {
    let area = centered(f.area(), 64, 10);
    let inner = panel(f, area, " new session ");

    let field_line = |label: &str, value: &str, focused: bool| {
        let marker = if focused { "▸" } else { " " };
        let mut spans = vec![
            Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{label:<6}"), Style::default().fg(Color::DarkGray)),
            Span::raw(value.to_string()),
        ];
        if focused {
            spans.push(Span::styled("█", Style::default().fg(Color::Cyan)));
        }
        Line::from(spans)
    };

    let mut lines = vec![
        Line::from(vec![
            Span::raw("   "),
            Span::styled("tool  ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("‹ {} ›", v.tool), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]),
        field_line("name", v.name, v.field == 0),
        field_line("dir", v.dir, v.field == 1),
        Line::from(""),
    ];
    if let Some(e) = v.error {
        lines.push(Line::from(Span::styled(format!(" {e}"), Style::default().fg(Color::Red))));
    }
    lines.push(Line::from(Span::styled(
        " ←/→ tool · tab field · enter create · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}

/// The layout palette: six box-glyph thumbnails (Layout::ALL order), the
/// `highlight` one in accent. Mirrors the web `LayoutPalette`.
pub fn layout_palette(f: &mut Frame, highlight: usize) {
    const GLYPHS: [[&str; 3]; 6] = [
        ["╭───╮", "│   │", "╰───╯"], // single
        ["╭───╮", "├───┤", "╰───╯"], // stacked
        ["╭─┬─╮", "│ │ │", "╰─┴─╯"], // side-by-side
        ["╭─┬─╮", "│ ├─┤", "╰─┴─╯"], // left-L
        ["╭─┬─╮", "├─┤ │", "╰─┴─╯"], // right-L
        ["╭─┬─╮", "├─┼─┤", "╰─┴─╯"], // quad
    ];
    const LABELS: [&str; 6] = ["single", "stack", "cols", "left-L", "right-L", "quad"];

    let inner = panel(f, centered(f.area(), 54, 10), " layout ");

    let hi = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let style = |g: usize| if g == highlight { hi } else { dim };

    let row = |cell: &dyn Fn(usize) -> String| {
        let mut spans = vec![Span::raw(" ")];
        for g in 0..6 {
            spans.push(Span::styled(cell(g), style(g)));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    };

    let mut lines = vec![Line::from("")];
    for r in 0..3 {
        lines.push(row(&|g| format!(" {} ", GLYPHS[g][r])));
    }
    lines.push(row(&|g| format!("{:^7}", LABELS[g])));
    lines.push(row(&|g| {
        let d = if g == highlight { format!("[{}]", g + 1) } else { format!("{}", g + 1) };
        format!("{d:^7}")
    }));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " ←/→ move · 1-6 jump · enter apply · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(Paragraph::new(lines), inner);
}

/// What the unified grid action menu needs to render. `selected` indexes the
/// same flat list as `app::menu_items`: 0 = Change layout, 1 = New session,
/// `2..2+n` = the sessions, `2+n` = Clear this box, `3+n` = Quit.
pub struct MenuView<'a> {
    pub sessions: &'a [SessionInfo],
    pub selected: usize,
    pub box_num: usize,
    pub box_count: usize,
}

fn menu_marker(sel: bool) -> Span<'static> {
    Span::styled(
        format!("{} ", if sel { "▸" } else { " " }),
        Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
    )
}

/// The unified grid action menu — a centered list over the grid (so the boxes
/// stay visible). Change layout / New session sit above the session list (the
/// marker starts on the box's current session), with Clear this box / Quit
/// below. Reached via `Ctrl-A d` or Enter/click on an empty box.
pub fn grid_menu(f: &mut Frame, v: &MenuView) {
    const MAX_SESS: usize = 8;
    let n = v.sessions.len();
    let shown = n.min(MAX_SESS);
    let sel_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let gray = Style::default().fg(Color::Gray);

    let action = |flat: usize, glyph: &str, label: &str| {
        let sel = flat == v.selected;
        Line::from(vec![
            menu_marker(sel),
            Span::styled(format!("{glyph}  {label}"), if sel { sel_style } else { gray }),
        ])
    };
    let sep = || Line::from(Span::styled("   ──────────────────────────────", dim));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(action(0, "▦", "Change layout"));
    lines.push(action(1, "✚", "New session"));
    lines.push(sep());
    if n == 0 {
        lines.push(Line::from(Span::styled("     no sessions", dim)));
    } else {
        // Window the session range around the cursor when it's inside it.
        let sel_sess = v.selected.checked_sub(2).filter(|&i| i < n);
        let start = match sel_sess {
            Some(i) => i.saturating_sub(MAX_SESS - 1).min(n - shown),
            None => 0,
        };
        for (i, s) in v.sessions.iter().enumerate().skip(start).take(shown) {
            let sel = 2 + i == v.selected;
            let dot = if s.attached { "●" } else { "○" };
            lines.push(Line::from(vec![
                menu_marker(sel),
                Span::styled(
                    format!("{dot} "),
                    Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
                ),
                Span::styled(format!("{:<22}", truncate(&s.name, 22)), if sel { sel_style } else { gray }),
                Span::styled(format!("{:<7}", truncate(&s.tool, 7)), dim),
                Span::styled(truncate(&s.preview, 18), dim),
            ]));
        }
    }
    lines.push(sep());
    lines.push(action(2 + n, "✕", "Clear this box"));
    lines.push(action(3 + n, "⏻", "Quit ccs"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" ↑↓ move · ⏎ select · esc cancel", dim)));

    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), 64, h), &format!(" box {}/{} ", v.box_num, v.box_count));
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_to<F: FnOnce(&mut Frame)>(w: u16, h: u16, draw: F) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f)).unwrap();
        t.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn confirm_renders_body_and_keys() {
        let s = render_to(70, 14, |f| confirm(f, " confirm ", "kill session claude-x?"));
        assert!(s.contains("kill session claude-x?"), "{s}");
        assert!(s.contains("y confirm"), "{s}");
    }

    fn sess(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: "claude".into(),
            short: name.into(),
            attached: false,
            activity: 0,
            preview: "p".into(),
            cwd: String::new(),
        }
    }

    #[test]
    fn grid_menu_lists_actions_and_sessions() {
        let list = vec![sess("claude-a"), sess("codex-b")];
        // selected = 2 → the first session is highlighted.
        let s = render_to(72, 18, |f| {
            grid_menu(f, &MenuView { sessions: &list, selected: 2, box_num: 2, box_count: 4 })
        });
        assert!(s.contains("box 2/4"), "{s}");
        assert!(s.contains("Change layout"), "{s}");
        assert!(s.contains("New session"), "{s}");
        assert!(s.contains("Clear this box"), "{s}");
        assert!(s.contains("Quit ccs"), "{s}");
        assert!(s.contains("claude-a"), "{s}");
        assert!(s.contains("codex-b"), "{s}");
        assert!(s.contains('▸'), "selection marker: {s}");
    }

    #[test]
    fn grid_menu_empty_still_shows_actions() {
        let s = render_to(72, 14, |f| {
            grid_menu(f, &MenuView { sessions: &[], selected: 1, box_num: 1, box_count: 1 })
        });
        assert!(s.contains("no sessions"), "{s}");
        assert!(s.contains("New session"), "{s}");
        assert!(s.contains("Quit ccs"), "{s}");
    }

    #[test]
    fn layout_palette_renders_glyphs_and_highlight() {
        let s = render_to(60, 14, |f| layout_palette(f, 5)); // quad highlighted
        assert!(s.contains("layout"), "{s}");
        assert!(s.contains("single"), "{s}");
        assert!(s.contains("quad"), "{s}");
        assert!(s.contains("[6]"), "highlighted digit: {s}");
    }

    #[test]
    fn new_session_renders_fields_and_error() {
        let v = NewSessionView {
            tool: "claude",
            name: "proj",
            dir: "/home/u",
            field: 0,
            error: Some("already exists"),
        };
        let s = render_to(70, 14, |f| new_session(f, &v));
        assert!(s.contains("new session"), "{s}");
        assert!(s.contains("claude"), "{s}");
        assert!(s.contains("proj"), "{s}");
        assert!(s.contains("/home/u"), "{s}");
        assert!(s.contains("already exists"), "{s}");
    }
}
