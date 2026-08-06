//! The session switcher: header, search query line, scrollable session list
//! (grouped by machine in multi-machine hub mode), the selected session's
//! `detail` footer, and the status bar. Search-first per proposal 0062: typing
//! filters, so all letter commands live on Ctrl-chords.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, SwitcherRow};
use crate::ui::util::{ago, color_token_to_color, dir_crumb, truncate};

const BAR_BG: Color = Color::Rgb(15, 23, 32);
const SEL_BG: Color = Color::Rgb(30, 41, 59);

/// Below this many terminal rows the `detail` footer is suppressed entirely —
/// the footer is an enhancement, the list is essential (proposal 0062).
const MIN_ROWS_FOR_DETAIL: u16 = 12;

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();
    // The selected session's 2–3 sentence `detail` (proposal 0022), the TUI
    // analogue of the web's SummaryTip hover: 0 rows (absent/short terminal),
    // else 1–2 rows depending on how much it wraps.
    let detail = if area.height >= MIN_ROWS_FOR_DETAIL {
        app.selected_session().and_then(|s| s.detail.as_deref()).filter(|d| !d.is_empty())
    } else {
        None
    };
    let detail_width = area.width.saturating_sub(3).max(1) as usize;
    let footer_rows = match detail {
        None => 0,
        Some(d) if d.chars().count() <= detail_width => 1,
        Some(_) => 2,
    };

    let rows = Layout::vertical([
        Constraint::Length(1),           // header
        Constraint::Length(1),           // search query
        Constraint::Min(1),              // list
        Constraint::Length(footer_rows), // selected session's detail
        Constraint::Length(1),           // status bar
    ])
    .split(area);

    render_header(f, rows[0]);
    render_query(f, rows[1], app);

    if app.sessions().is_empty() {
        let hint = Paragraph::new("  no sessions · ^N new · ^O restore · Esc quit")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, rows[2]);
    } else if app.filtering() && app.visible_sessions().is_empty() {
        let hint = Paragraph::new("  no matches — Esc to clear")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, rows[2]);
    } else {
        render_list(f, rows[2], app);
    }

    if let Some(d) = detail {
        render_detail(f, rows[3], d);
    }
    render_status(f, rows[4], app);
}

fn render_header(f: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            " cc-screen ",
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  sessions", Style::default().fg(Color::Gray)),
        Span::styled(
            "   type to search · ↑↓ ⏎ attach · ^N new · ^X kill · ^E exit · ^R rename · ^O restore",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The always-visible search line (proposal 0062 Part A) — the TUI analogue of
/// the web sidebar's autofocused box. Truncates from the LEFT on narrow
/// terminals so the tail of the query (where the cursor is) stays visible.
fn render_query(f: &mut Frame, area: Rect, app: &App) {
    let q = app.query();
    let mut spans = vec![Span::styled(" › ", Style::default().fg(Color::Cyan))];
    if q.is_empty() {
        spans.push(Span::styled(
            "type to search",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ));
    } else {
        let avail = area.width.saturating_sub(5).max(1) as usize; // prefix + cursor
        let len = q.chars().count();
        let shown: String =
            if len > avail { q.chars().skip(len - avail).collect() } else { q.to_string() };
        spans.push(Span::styled(shown, Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_list(f: &mut Frame, area: Rect, app: &App) {
    // Content width available to a row = the list width minus the 2-col
    // highlight symbol ("▸ ") the List reserves. Used to bound the trailing
    // headline/preview so a row never wraps (it clips) — holds at 40 cols.
    let row_width = area.width.saturating_sub(2) as usize;
    // While filtering across machines the group headers are suppressed (a flat
    // ranked list reads better), so each row carries an inline machine chip.
    let chip = app.filtering() && app.multi_machine();
    let rows = app.switcher_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            SwitcherRow::Header { label, online } => header_item(label, *online, row_width),
            SwitcherRow::Session(i) => session_item(app, *i, chip, row_width),
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    state.select(selected_row(&rows, app.selected()));
    f.render_stateful_widget(list, area, &mut state);
}

/// A per-machine group header row (proposal 0062 Part B) — non-selectable, so
/// it maps to no `visible_sessions()` entry. Mirrors the web sidebar's
/// hostname headers, including the offline marker.
fn header_item(label: &str, online: bool, row_width: usize) -> ListItem<'static> {
    let mut spans = vec![Span::styled(
        format!("{} ", truncate(label, row_width.saturating_sub(12).max(4))),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    )];
    if !online {
        spans.push(Span::styled(
            "· offline",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn session_item(app: &App, idx: usize, chip: bool, row_width: usize) -> ListItem<'static> {
    let s = &app.sessions()[idx];
    // A web-set session colour (proposal 0029) tints the attach dot as the
    // row's accent; absent/unknown token → today's green/grey (no
    // regression). Display-only: the TUI never sets a colour.
    let accent = color_token_to_color(s.color.as_deref());
    let (dot, dot_color) = if s.attached {
        ("●", accent.unwrap_or(Color::Green))
    } else {
        ("○", accent.unwrap_or(Color::DarkGray))
    };
    // `waiting` is the resting state for an idle agent, so we surface the
    // inverse: an amber marker on sessions still producing output. A
    // glance then shows which agents are working vs done — mirrors the
    // web PWA's "running" badge. (See the server's WORK_GRACE_SECS.)
    let work = if s.waiting { "  " } else { "● " };
    // Prefer the operator display label (0059 C1) when set; otherwise the
    // folder breadcrumb (parent/leaf from the live cwd, proposal 0025) so
    // two same-named sessions in different dirs read apart at a glance —
    // itself falling back to the slug with no cwd.
    let named = match s.label.as_deref() {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => dir_crumb(&s.cwd, &s.name),
    };
    let mut spans = vec![
        Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
        Span::styled(
            format!("{:<26}", truncate(&named, 26)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{:<8}", truncate(&s.tool, 8)), Style::default().fg(Color::Cyan)),
        Span::styled(format!("{:>5}  ", ago(s.activity)), Style::default().fg(Color::DarkGray)),
        Span::styled(work, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    ];
    // Badge the one remaining non-default policy state: the rare session
    // launched with normal permission prompts (0014 removed view-only).
    let mut prefix_cols = 2 + 26 + 8 + 7 + 2; // dot+crumb+tool+ago+work
    if s.skip_permissions == Some(false) {
        spans.push(Span::styled(
            "safe ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ));
        prefix_cols += 5;
    }
    // The inline machine chip (filtering, multi-machine): the group headers are
    // suppressed, so the row itself says where the session lives.
    if chip && !s.machine.is_empty() {
        let host = truncate(&app.machine_label(&s.machine), 12);
        prefix_cols += host.chars().count() + 3;
        spans.push(Span::styled(format!("[{host}] "), Style::default().fg(Color::DarkGray)));
    }
    // Trailing summary (proposal 0062 Part C, web-parity emphasis): the LLM
    // `headline` (≤6 words, 0022/0059 C4) in the normal summary grey; the raw
    // `preview` fallback visibly raw — darker + italic — so terminal noise
    // never outshines (or masquerades as) an AI summary. Neither → "—".
    // Bounded to the remaining row width so the line clips rather than wraps.
    let avail = row_width.saturating_sub(prefix_cols);
    if avail > 1 {
        let (tail, style) = match s.headline.as_deref() {
            Some(h) if !h.is_empty() => (h, Style::default().fg(Color::Gray)),
            _ if !s.preview.is_empty() => (
                s.preview.as_str(),
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ),
            _ => ("—", Style::default().fg(Color::DarkGray)),
        };
        spans.push(Span::styled(truncate(tail, avail), style));
    }
    ListItem::new(Line::from(spans))
}

/// Map the cursor (an index into `visible_sessions()`) to its row in the
/// display list, skipping the non-selectable group headers.
fn selected_row(rows: &[SwitcherRow], selected: usize) -> Option<usize> {
    let mut nth = 0usize;
    for (ri, row) in rows.iter().enumerate() {
        if matches!(row, SwitcherRow::Session(_)) {
            if nth == selected {
                return Some(ri);
            }
            nth += 1;
        }
    }
    None
}

/// The selected session's `detail` footer (proposal 0062 Part C): wrapped and
/// clipped to at most 2 rows above the status bar. Collapsed (0 rows) when the
/// selection has no detail — never duplicates the headline.
fn render_detail(f: &mut Frame, area: Rect, detail: &str) {
    let inner = Rect {
        x: area.x + 3,
        y: area.y,
        width: area.width.saturating_sub(3),
        height: area.height,
    };
    let p = Paragraph::new(detail.to_string())
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true });
    f.render_widget(p, inner);
}

fn render_status(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let bar = Paragraph::new(Line::from(format!(" {}", app.status())))
        .style(Style::default().bg(BAR_BG).fg(Color::Gray));
    f.render_widget(bar, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use cc_screen_protocol::SessionInfo;
    use ratatui::{backend::TestBackend, Terminal};

    fn sess(name: &str, tool: &str, attached: bool, waiting: bool, preview: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: tool.into(),
            short: name.into(),
            attached,
            activity: 0,
            last_input_at: 0,
            busy_since: 0,
            busy_until: 0,
            preview: preview.into(),
            waiting,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
            label: None,
        }
    }

    fn rendered(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn lists_sessions_with_header_and_bar() {
        let app = App::test_fixture(
            vec![
                sess("claude-myproj", "claude", true, false, "Running tests"),
                sess("codex-api", "codex", false, true, "Done."),
            ],
            "2 session(s) · http://127.0.0.1:8839",
        );
        let text = rendered(&app, 90, 8);
        assert!(text.contains("cc-screen"), "header missing:\n{text}");
        assert!(text.contains("claude-myproj"), "session 1 missing:\n{text}");
        assert!(text.contains("codex-api"), "session 2 missing:\n{text}");
        assert!(text.contains("Running tests"), "preview missing:\n{text}");
        assert!(text.contains("2 session(s)"), "status bar missing:\n{text}");
    }

    #[test]
    fn working_session_shows_amber_marker() {
        // Both sessions are detached (leading "○"), so the only "●" in the frame
        // is the working marker — present for the active agent, absent for the
        // idle/waiting one.
        let working = App::test_fixture(vec![sess("claude-x", "claude", false, false, "thinking")], "s");
        let idle = App::test_fixture(vec![sess("claude-x", "claude", false, true, "done")], "s");
        assert!(rendered(&working, 90, 6).contains('●'), "working row should show the ● marker");
        assert!(!rendered(&idle, 90, 6).contains('●'), "idle/waiting row should not show the ● marker");
    }

    #[test]
    fn empty_list_shows_hint() {
        let app = App::test_fixture(vec![], "0 session(s) · http://127.0.0.1:8839");
        let text = rendered(&app, 80, 6);
        assert!(text.contains("no sessions"), "empty hint missing:\n{text}");
    }

    // ── proposal 0062 ────────────────────────────────────────────────────────

    fn machine(id: &str, hostname: &str, online: bool) -> cc_screen_protocol::MachineInfo {
        cc_screen_protocol::MachineInfo { machine: id.into(), hostname: hostname.into(), online }
    }

    fn sess_on(name: &str, m: &str) -> SessionInfo {
        let mut s = sess(name, "claude", false, true, "");
        s.machine = m.into();
        s
    }

    /// Render and return the style of the cell where `needle` starts.
    fn style_at(app: &App, w: u16, h: u16, needle: &str) -> ratatui::style::Style {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        let buf = term.backend().buffer();
        let area = buf.area;
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<Vec<_>>().join("");
            if let Some(col) = row.find(needle) {
                // `find` returns a byte offset; rows here are 1-byte-symbol cells
                // up to the needle in these fixtures (ASCII prefixes).
                let x = row[..col].chars().count() as u16;
                return buf[(x, y)].style();
            }
        }
        panic!("needle {needle:?} not found in:\n{}", rendered(app, w, h));
    }

    #[test]
    fn query_line_renders_typed_text() {
        let mut app = App::test_fixture(vec![sess("alpha", "claude", false, true, "")], "s");
        assert!(rendered(&app, 90, 8).contains("type to search"), "empty query shows the hint");
        app.test_set_query("alp");
        let text = rendered(&app, 90, 8);
        assert!(text.contains("› alp"), "typed query missing:\n{text}");
    }

    #[test]
    fn headline_and_preview_styles_are_distinct() {
        // Part C: the AI headline gets the summary emphasis; the raw preview
        // fallback is visibly raw (darker + italic) — not a DIM-only split.
        let mut with_headline = sess("one", "claude", false, true, "raw noise");
        with_headline.headline = Some("summarised work".into());
        let raw_only = sess("two", "claude", false, true, "rawtail");
        let app = App::test_fixture(vec![with_headline, raw_only], "s");
        let h = style_at(&app, 100, 8, "summarised");
        let p = style_at(&app, 100, 8, "rawtail");
        assert_ne!(h, p, "headline and preview must not share a style");
        assert!(p.add_modifier.contains(Modifier::ITALIC), "preview is italic");
        assert!(!h.add_modifier.contains(Modifier::ITALIC), "headline is not italic");
        assert_eq!(h.fg, Some(Color::Gray));
        assert_eq!(p.fg, Some(Color::DarkGray));
    }

    #[test]
    fn multi_machine_groups_under_headers_at_rest_and_chips_when_filtering() {
        let mut app = App::test_fixture_hub(
            vec![sess_on("alpha", "boxA"), sess_on("bravo", "boxB")],
            vec![machine("boxA", "hostA", true), machine("boxB", "hostB", false)],
            "2 session(s)",
        );
        let text = rendered(&app, 100, 12);
        assert!(text.contains("hostA"), "boxA header missing:\n{text}");
        assert!(text.contains("hostB"), "boxB header missing:\n{text}");
        assert!(text.contains("offline"), "offline marker missing:\n{text}");
        assert!(!text.contains('['), "no chips at rest:\n{text}");
        // Filtering: headers suppressed, surviving rows carry the machine chip.
        app.test_set_query("bravo");
        let text = rendered(&app, 100, 12);
        assert!(text.contains("[hostB]"), "machine chip missing:\n{text}");
        assert!(!text.contains("hostA"), "headers (and boxA's row) gone while filtering:\n{text}");
    }

    #[test]
    fn direct_mode_renders_without_headers_or_chips() {
        let app = App::test_fixture(
            vec![sess("alpha", "claude", false, true, ""), sess("beta", "codex", false, true, "")],
            "s",
        );
        let text = rendered(&app, 100, 12);
        assert!(!text.contains('['), "no chips in direct mode:\n{text}");
        // Rows follow the header + query lines directly (no group header rows).
        assert!(text.contains("alpha") && text.contains("beta"));
    }

    #[test]
    fn selected_detail_shows_in_footer_and_collapses_without_one() {
        let mut detailed = sess("alpha", "claude", false, true, "");
        detailed.detail = Some("Working through the parser refactor and its tests.".into());
        let plain = sess("beta", "claude", false, true, "");
        let mut app = App::test_fixture(vec![detailed, plain], "s");
        let text = rendered(&app, 100, 14);
        assert!(text.contains("parser refactor"), "detail footer missing:\n{text}");
        // Selecting the detail-less session collapses the footer.
        app.test_set_selected(1);
        let text = rendered(&app, 100, 14);
        assert!(!text.contains("parser refactor"), "footer should collapse:\n{text}");
    }

    #[test]
    fn detail_footer_suppressed_on_short_terminals() {
        // At 80×10 the list keeps its space: no footer, no panic, no overlap.
        let mut detailed = sess("alpha", "claude", false, true, "");
        detailed.detail = Some("A detail that would need a footer row to show.".into());
        let app = App::test_fixture(vec![detailed], "9 session(s)");
        let text = rendered(&app, 80, 10);
        assert!(!text.contains("footer row"), "detail must be suppressed at 10 rows:\n{text}");
        assert!(text.contains("alpha") && text.contains("session(s)"));
    }

    #[test]
    fn narrow_terminal_renders_query_and_headers_without_panic() {
        // 40×15 (the terminal-environments budget): query line + a group header
        // + rows all render; the query truncates from the left (tail visible).
        let mut app = App::test_fixture_hub(
            vec![sess_on("alpha", "boxA"), sess_on("bravo", "boxB")],
            vec![machine("boxA", "hostA", true), machine("boxB", "hostB", true)],
            "s",
        );
        let text = rendered(&app, 40, 15);
        assert!(text.contains("hostA"), "header at 40 cols:\n{text}");
        app.test_set_query(&"x".repeat(60));
        let text = rendered(&app, 40, 15);
        assert!(text.contains("xxx"), "query tail renders at 40 cols:\n{text}");
    }
}
