//! The tiled grid view: each box is a session's emulator, bordered with its
//! name; the focused box gets an accent border. `Single` keeps the clean
//! borderless full-screen look. A shared bottom bar shows the focused box.

use cc_screen_protocol::SessionInfo;
use ratatui::{
    layout::{Alignment, Constraint, Layout as RLayout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::layout::{bordered, tiles, Layout};
use crate::pane::Pane;
use crate::ui::statusbar;

const FOCUS: Color = Color::Cyan;
const DIM_BORDER: Color = Color::Rgb(60, 70, 85);

#[allow(clippy::too_many_arguments)]
pub fn render(
    f: &mut Frame,
    layout: Layout,
    panes: &[Option<Pane>],
    // The live session list, so a pane's title picks up an operator rename on the
    // next poll (0059 C1) — the box holds the identity name, the label is resolved
    // here at render time.
    sessions: &[SessionInfo],
    active: usize,
    prefix_label: &str,
    prefix_armed: bool,
    scroll_mode: bool,
    toast: Option<&str>,
    // A transient statusbar note (0069 Part D) — outranked by the ready-toast.
    hint: Option<&str>,
) {
    let rows = RLayout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
    let body = rows[0];

    let single = !bordered(layout);
    for (i, rect) in tiles(layout, body).into_iter().enumerate() {
        let pane = panes.get(i).and_then(|p| p.as_ref());
        render_box(f, rect, pane, sessions, i == active, single);
    }

    let focused = panes.get(active).and_then(|p| p.as_ref());
    statusbar::render(
        f, rows[1], focused, sessions, layout, active, panes.len(), prefix_label, prefix_armed,
        scroll_mode, toast, hint,
    );
}

/// A pane's box title: `machine/label` when aggregated through a hub, else the
/// label — resolving the operator display name (0059 C1) by identity from the
/// session list, falling back to the pane's session name when it isn't listed.
fn pane_title(p: &Pane, sessions: &[SessionInfo]) -> String {
    let base = sessions
        .iter()
        .find(|s| s.name == p.session && s.machine == p.machine)
        .map(crate::app::display_name)
        .unwrap_or(p.session.as_str());
    if p.machine.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", p.machine, base)
    }
}

fn render_box(
    f: &mut Frame,
    rect: Rect,
    pane: Option<&Pane>,
    sessions: &[SessionInfo],
    focused: bool,
    single: bool,
) {
    if single {
        match pane {
            Some(p) => f.render_widget(p, rect),
            None => f.render_widget(empty_hint(), rect),
        }
        return;
    }

    // A web-set session mark colour (proposal 0029) accents the box border; the
    // focused box keeps its bold title. Unmarked → today's cyan-focus / dim-grey.
    // Display-only: the accent is mirrored from the session list, never set here.
    let accent = pane.and_then(Pane::accent);
    let (bs, ts) = match (focused, accent) {
        (true, Some(c)) => {
            (Style::default().fg(c), Style::default().fg(c).add_modifier(Modifier::BOLD))
        }
        (true, None) => {
            (Style::default().fg(FOCUS), Style::default().fg(FOCUS).add_modifier(Modifier::BOLD))
        }
        (false, Some(c)) => (Style::default().fg(c), Style::default().fg(Color::Gray)),
        (false, None) => (Style::default().fg(DIM_BORDER), Style::default().fg(Color::Gray)),
    };
    let title = match pane {
        Some(p) => pane_title(p, sessions),
        None => "empty".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(bs)
        .title(Span::styled(format!(" {title} "), ts));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    match pane {
        Some(p) => f.render_widget(p, inner),
        None => f.render_widget(empty_hint(), inner),
    }
}

fn empty_hint() -> Paragraph<'static> {
    Paragraph::new("⏎ for menu")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::Pane;
    use ratatui::{backend::TestBackend, Terminal};
    use tokio::sync::mpsc;

    fn dummy(id: u64, name: &str) -> Pane {
        let (tx, _rx) = mpsc::channel(4);
        Pane::new(id, name.into(), String::new(), 40, 10, tx, tokio::spawn(async {}))
    }

    #[tokio::test]
    async fn quad_shows_titles_hints_and_bar() {
        let panes = vec![Some(dummy(1, "shell-a")), None, None, None];
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        t.draw(|f| render(f, Layout::Quad, &panes, &[], 0, "^A", false, false, None, None))
            .unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains("shell-a"), "filled box title: {s:?}");
        assert!(s.contains("for menu"), "empty box hint: {s:?}");
        assert!(s.contains("quad"), "bar layout label: {s:?}");
        assert!(s.contains("box 1/4"), "bar focus indicator: {s:?}");
    }
}
