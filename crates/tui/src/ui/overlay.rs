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

/// Scoped session picker for filling a grid box — a centered list over the
/// grid, so you don't lose the boxes while choosing.
pub fn session_picker(
    f: &mut Frame,
    sessions: &[SessionInfo],
    selected: usize,
    box_num: usize,
    box_count: usize,
) {
    const MAX_ROWS: usize = 10;
    let shown = sessions.len().min(MAX_ROWS);
    let h = (shown.max(1) + 4) as u16;
    let inner = panel(f, centered(f.area(), 64, h), &format!(" pick · box {box_num}/{box_count} "));

    let mut lines: Vec<Line> = Vec::new();
    if sessions.is_empty() {
        lines.push(Line::from(Span::styled("  no sessions — press n to create", Style::default().fg(Color::DarkGray))));
    } else {
        // Window the list so the selection stays visible.
        let start = selected.saturating_sub(MAX_ROWS - 1).min(sessions.len().saturating_sub(shown));
        for (i, s) in sessions.iter().enumerate().skip(start).take(shown) {
            let sel = i == selected;
            let dot = if s.attached { "●" } else { "○" };
            let name_style = if sel {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {dot} ", if sel { "▸" } else { " " }),
                    Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
                ),
                Span::styled(format!("{:<24}", truncate(&s.name, 24)), name_style),
                Span::styled(format!("{:<7}", truncate(&s.tool, 7)), Style::default().fg(Color::DarkGray)),
                Span::styled(truncate(&s.preview, 20), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " ↑↓ pick · ⏎ attach · n new · esc cancel",
        Style::default().fg(Color::DarkGray),
    )));
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
    fn session_picker_lists_and_highlights() {
        let list = vec![sess("claude-a"), sess("codex-b")];
        let s = render_to(70, 12, |f| session_picker(f, &list, 1, 2, 4));
        assert!(s.contains("pick"), "{s}");
        assert!(s.contains("box 2/4"), "{s}");
        assert!(s.contains("claude-a"), "{s}");
        assert!(s.contains("codex-b"), "{s}");
        assert!(s.contains('▸'), "selection marker: {s}");
    }

    #[test]
    fn session_picker_empty_prompts_new() {
        let s = render_to(70, 8, |f| session_picker(f, &[], 0, 1, 1));
        assert!(s.contains("no sessions"), "{s}");
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
