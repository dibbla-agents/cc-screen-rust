//! The session switcher: header, scrollable session list, status bar.

use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::App;
use crate::ui::util::{ago, color_token_to_color, dir_crumb, truncate};

const BAR_BG: Color = Color::Rgb(15, 23, 32);
const SEL_BG: Color = Color::Rgb(30, 41, 59);

pub fn render(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(1),    // list
        Constraint::Length(1), // status bar
    ])
    .split(f.area());

    render_header(f, rows[0]);

    if app.sessions().is_empty() {
        let hint = Paragraph::new("  no sessions · n new · R restore · q quit")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(hint, rows[1]);
    } else {
        render_list(f, rows[1], app);
    }

    render_status(f, rows[2], app);
}

fn render_header(f: &mut Frame, area: ratatui::layout::Rect) {
    let line = Line::from(vec![
        Span::styled(
            " cc-screen ",
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  sessions", Style::default().fg(Color::Gray)),
        Span::styled(
            "   ↑↓ · ⏎ attach · n new · x kill · e exit · R rename/restore · q quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_list(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    // Content width available to a row = the list width minus the 2-col
    // highlight symbol ("▸ ") the List reserves. Used to bound the trailing
    // headline/preview so a row never wraps (it clips) — holds at 40 cols.
    let row_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .sessions()
        .iter()
        .map(|s| {
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
            // Trailing summary: the LLM `headline` (≤6 words, proposal 0059 C4)
            // when present — rendered dim, so it reads as secondary — else today's
            // raw `preview`. Bounded to the remaining row width so the line clips
            // rather than wraps (holds at 40 cols).
            let avail = row_width.saturating_sub(prefix_cols);
            if avail > 1 {
                let (tail, style) = match s.headline.as_deref() {
                    Some(h) if !h.is_empty() => {
                        (h, Style::default().fg(Color::Gray).add_modifier(Modifier::DIM))
                    }
                    _ => (s.preview.as_str(), Style::default().fg(Color::Gray)),
                };
                spans.push(Span::styled(truncate(tail, avail), style));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");

    let mut state = ListState::default();
    state.select(Some(app.selected()));
    f.render_stateful_widget(list, area, &mut state);
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
}
