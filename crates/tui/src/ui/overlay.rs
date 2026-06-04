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

use crate::app::FormField;
use crate::client::DirEntry;
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
    /// The machine to show; `None` hides the row (unknown / no name).
    pub machine: Option<&'a str>,
    pub machine_online: bool,
    /// True in hub mode (the row is a `‹ › ` picker); false in direct mode (a
    /// read-only label — only one machine, nothing to pick).
    pub machine_pickable: bool,
    pub name: &'a str,
    pub dir: &'a str,
    pub focus: FormField,
    /// Dir autocomplete candidates + the highlighted one (shown under the dir).
    pub candidates: &'a [DirEntry],
    pub cand_sel: Option<usize>,
    pub error: Option<&'a str>,
}

/// How many dir candidates to show at once.
const MAX_DIR_CANDS: usize = 6;

/// The new-session form: tool + (machine) selectors, name + dir text fields, with
/// the focused field marked, and — under the dir — a live directory autocomplete.
pub fn new_session(f: &mut Frame, v: &NewSessionView) {
    let accent = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);

    // A selector row: `label  ‹ value ›`, the value accented when focused.
    let selector = |label: &str, value: &str, focused: bool, suffix: Vec<Span<'static>>| {
        let marker = if focused { "▸" } else { " " };
        let mut spans = vec![
            Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{label:<6}"), dim),
            Span::styled(format!("‹ {value} ›"), accent),
        ];
        spans.extend(suffix);
        Line::from(spans)
    };
    // A text-entry row: `label  value█`, the cursor block shown when focused.
    let field_line = |label: &str, value: &str, focused: bool| {
        let marker = if focused { "▸" } else { " " };
        let mut spans = vec![
            Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{label:<6}"), dim),
            Span::raw(value.to_string()),
        ];
        if focused {
            spans.push(Span::styled("█", Style::default().fg(Color::Cyan)));
        }
        Line::from(spans)
    };

    // A read-only label row: `label  value` (no marker, no selector arrows).
    let label_line = |label: &str, value: &str, suffix: Vec<Span<'static>>| {
        let mut spans = vec![
            Span::raw("   "),
            Span::styled(format!("{label:<6}"), dim),
            Span::styled(value.to_string(), Style::default().fg(Color::Gray)),
        ];
        spans.extend(suffix);
        Line::from(spans)
    };

    let mut lines = vec![selector("tool", v.tool, v.focus == FormField::Tool, vec![])];
    if let Some(m) = v.machine {
        let suffix = if v.machine_online {
            vec![]
        } else {
            vec![Span::styled("  offline", Style::default().fg(Color::Red))]
        };
        if v.machine_pickable {
            lines.push(selector("machine", m, v.focus == FormField::Machine, suffix));
        } else {
            lines.push(label_line("machine", m, suffix));
        }
    }
    lines.push(field_line("name", v.name, v.focus == FormField::Name));
    lines.push(field_line("dir", v.dir, v.focus == FormField::Dir));

    // Dir autocomplete: a windowed list under the dir field, indented to align
    // under the value, the highlighted candidate accented.
    let dir_focused = v.focus == FormField::Dir;
    if dir_focused && !v.candidates.is_empty() {
        let shown = v.candidates.len().min(MAX_DIR_CANDS);
        // Keep the highlighted candidate in view.
        let start = match v.cand_sel {
            Some(i) if i >= shown => i + 1 - shown,
            _ => 0,
        };
        for (i, c) in v.candidates.iter().enumerate().skip(start).take(shown) {
            let sel = v.cand_sel == Some(i);
            let marker = if sel { "›" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(format!("       {marker} "), Style::default().fg(Color::Cyan)),
                Span::styled(c.name.clone(), if sel { accent } else { Style::default().fg(Color::Gray) }),
            ]));
        }
        if v.candidates.len() > shown {
            lines.push(Line::from(Span::styled(
                format!("         … {} more", v.candidates.len() - shown),
                dim,
            )));
        }
    }

    lines.push(Line::from(""));
    if let Some(e) = v.error {
        lines.push(Line::from(Span::styled(format!(" {e}"), Style::default().fg(Color::Red))));
    }
    let hint = if dir_focused {
        " ↑↓ pick · tab/→ open · enter create · esc cancel"
    } else {
        " ←/→ change · tab field · enter create · esc cancel"
    };
    lines.push(Line::from(Span::styled(hint, dim)));

    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), 64, h), " new session ");
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

    // ~75% of the terminal width (the box is the focus while it's open), clamped
    // to something usable on tiny terminals and never wider than the screen.
    let area_w = f.area().width;
    let w = (area_w as u32 * 3 / 4).max(40).min(area_w as u32) as u16;
    let inner_w = w.saturating_sub(2) as usize; // panel borders take one col each side

    let action = |flat: usize, glyph: &str, label: &str| {
        let sel = flat == v.selected;
        Line::from(vec![
            menu_marker(sel),
            Span::styled(format!("{glyph}  {label}"), if sel { sel_style } else { gray }),
        ])
    };
    // A divider that spans the inner width (inset one column each side).
    let sep = || Line::from(Span::styled(format!(" {} ", "─".repeat(inner_w.saturating_sub(2))), dim));

    // Size the name column to the longest name so names show in full; the tool
    // column is fixed and the preview takes whatever's left (it's the croppable
    // one). lead = marker (2) + dot (2); the two inter-column gaps add 2 more.
    const TOOL_W: usize = 7;
    let lead = 4usize;
    let longest = v.sessions.iter().map(|s| s.name.chars().count()).max().unwrap_or(0);
    let name_cap = inner_w.saturating_sub(lead + 2 + TOOL_W + 8).max(12); // keep ≥8 for preview
    let name_w = longest.clamp(12, name_cap);
    let preview_w = inner_w.saturating_sub(lead + name_w + 2 + TOOL_W);

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
            let mut spans = vec![
                menu_marker(sel),
                Span::styled(
                    format!("{dot} "),
                    Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("{:<name_w$} ", truncate(&s.name, name_w)),
                    if sel { sel_style } else { gray },
                ),
                Span::styled(format!("{:<TOOL_W$} ", truncate(&s.tool, TOOL_W)), dim),
            ];
            if preview_w > 0 {
                spans.push(Span::styled(truncate(&s.preview, preview_w), dim));
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(sep());
    lines.push(action(2 + n, "✕", "Clear this box"));
    lines.push(action(3 + n, "⏻", "Quit ccs"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" ↑↓ move · ⏎ select · esc cancel", dim)));

    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), w, h), &format!(" box {}/{} ", v.box_num, v.box_count));
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
            waiting: false,
            cwd: String::new(),
            machine: String::new(),
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
            machine: None,
            machine_online: true,
            machine_pickable: true,
            name: "proj",
            dir: "/home/u",
            focus: FormField::Name,
            candidates: &[],
            cand_sel: None,
            error: Some("already exists"),
        };
        let s = render_to(70, 14, |f| new_session(f, &v));
        assert!(s.contains("new session"), "{s}");
        assert!(s.contains("claude"), "{s}");
        assert!(s.contains("proj"), "{s}");
        assert!(s.contains("/home/u"), "{s}");
        assert!(s.contains("already exists"), "{s}");
        // No machine → the machine row is absent.
        assert!(!s.contains("machine"), "{s}");
    }

    #[test]
    fn new_session_shows_machine_row_when_present() {
        let v = NewSessionView {
            tool: "claude",
            machine: Some("pine"),
            machine_online: true,
            machine_pickable: true,
            name: "proj",
            dir: "/home/u",
            focus: FormField::Machine,
            candidates: &[],
            cand_sel: None,
            error: None,
        };
        let s = render_to(70, 16, |f| new_session(f, &v));
        assert!(s.contains("machine"), "{s}");
        assert!(s.contains("pine"), "{s}");
        // Hub mode → rendered as a `‹ › ` picker.
        assert!(s.contains("‹ pine ›"), "{s}");
    }

    #[test]
    fn new_session_machine_label_readonly_in_direct_mode() {
        let v = NewSessionView {
            tool: "claude",
            machine: Some("pine"),
            machine_online: true,
            machine_pickable: false,
            name: "proj",
            dir: "/home/u",
            focus: FormField::Name,
            candidates: &[],
            cand_sel: None,
            error: None,
        };
        let s = render_to(70, 16, |f| new_session(f, &v));
        assert!(s.contains("machine"), "{s}");
        assert!(s.contains("pine"), "{s}");
        // Direct mode → plain label, no selector arrows.
        assert!(!s.contains("‹ pine ›"), "{s}");
    }

    #[test]
    fn new_session_lists_dir_candidates_when_dir_focused() {
        let cands = vec![
            DirEntry { name: "dev".into(), path: "/home/u/dev".into() },
            DirEntry { name: "docs".into(), path: "/home/u/docs".into() },
        ];
        let v = NewSessionView {
            tool: "claude",
            machine: None,
            machine_online: true,
            machine_pickable: true,
            name: "",
            dir: "/home/u/d",
            focus: FormField::Dir,
            candidates: &cands,
            cand_sel: Some(1),
            error: None,
        };
        let s = render_to(70, 18, |f| new_session(f, &v));
        assert!(s.contains("dev"), "{s}");
        assert!(s.contains("docs"), "{s}");
        assert!(s.contains('›'), "selection marker: {s}");
    }
}
