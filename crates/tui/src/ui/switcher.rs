//! The session switcher: header, scrollable session list, status bar.

use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::App;
use crate::ui::util::{ago, truncate};

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
        let hint = Paragraph::new("  no sessions — press q to quit (create lands in M4)")
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
            "   ↑↓ · ⏎ attach · n new · x kill · e exit · R restore · q quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_list(f: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let items: Vec<ListItem> = app
        .sessions()
        .iter()
        .map(|s| {
            let (dot, dot_color) = if s.attached {
                ("●", Color::Green)
            } else {
                ("○", Color::DarkGray)
            };
            let line = Line::from(vec![
                Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                Span::styled(
                    format!("{:<26}", truncate(&s.name, 26)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{:<8}", truncate(&s.tool, 8)), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:>5}  ", ago(s.activity)), Style::default().fg(Color::DarkGray)),
                Span::styled(truncate(&s.preview, 64), Style::default().fg(Color::Gray)),
            ]);
            ListItem::new(line)
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

    fn sess(name: &str, tool: &str, attached: bool, preview: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: tool.into(),
            short: name.into(),
            attached,
            activity: 0,
            preview: preview.into(),
            cwd: String::new(),
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
                sess("claude-myproj", "claude", true, "Running tests"),
                sess("codex-api", "codex", false, "Done."),
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
    fn empty_list_shows_hint() {
        let app = App::test_fixture(vec![], "0 session(s) · http://127.0.0.1:8839");
        let text = rendered(&app, 80, 6);
        assert!(text.contains("no sessions"), "empty hint missing:\n{text}");
    }
}
