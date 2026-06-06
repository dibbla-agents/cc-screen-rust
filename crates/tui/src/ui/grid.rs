//! The tiled grid view: each box is a session's emulator, bordered with its
//! name; the focused box gets an accent border. `Single` keeps the clean
//! borderless full-screen look. A shared bottom bar shows the focused box.

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

pub fn render(
    f: &mut Frame,
    layout: Layout,
    panes: &[Option<Pane>],
    active: usize,
    prefix_label: &str,
    prefix_armed: bool,
) {
    let rows = RLayout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
    let body = rows[0];

    let single = !bordered(layout);
    for (i, rect) in tiles(layout, body).into_iter().enumerate() {
        let pane = panes.get(i).and_then(|p| p.as_ref());
        render_box(f, rect, pane, i == active, single);
    }

    let focused = panes.get(active).and_then(|p| p.as_ref());
    statusbar::render(f, rows[1], focused, layout, active, panes.len(), prefix_label, prefix_armed);
}

fn render_box(f: &mut Frame, rect: Rect, pane: Option<&Pane>, focused: bool, single: bool) {
    if single {
        match pane {
            Some(p) => f.render_widget(p, rect),
            None => f.render_widget(empty_hint(), rect),
        }
        return;
    }

    let (bs, ts) = if focused {
        (Style::default().fg(FOCUS), Style::default().fg(FOCUS).add_modifier(Modifier::BOLD))
    } else {
        (Style::default().fg(DIM_BORDER), Style::default().fg(Color::Gray))
    };
    let title = match pane {
        // Mark a view-only box (0005) in its title so every box — not just the
        // focused one (the status bar covers that) — reads as view-only.
        Some(p) if p.view_only() => format!("{} ◌ view only", p.title()),
        Some(p) => p.title(),
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
        dummy_rc(id, name, None)
    }

    fn dummy_rc(id: u64, name: &str, rc: Option<bool>) -> Pane {
        let (tx, _rx) = mpsc::channel(4);
        Pane::new(id, name.into(), String::new(), 40, 10, tx, tokio::spawn(async {}), rc)
    }

    #[tokio::test]
    async fn view_only_box_titled_in_grid() {
        // A view-only pane (remote_control = Some(false)) wears a marker in its
        // box title; a controllable one does not (0005).
        let panes = vec![Some(dummy_rc(1, "shell-vo", Some(false))), Some(dummy(2, "shell-ok")), None, None];
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        t.draw(|f| render(f, Layout::Quad, &panes, 1, "^A", false)).unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains("view only"), "view-only box should be marked: {s:?}");
    }

    #[tokio::test]
    async fn quad_shows_titles_hints_and_bar() {
        let panes = vec![Some(dummy(1, "shell-a")), None, None, None];
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        t.draw(|f| render(f, Layout::Quad, &panes, 0, "^A", false)).unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains("shell-a"), "filled box title: {s:?}");
        assert!(s.contains("for menu"), "empty box hint: {s:?}");
        assert!(s.contains("quad"), "bar layout label: {s:?}");
        assert!(s.contains("box 1/4"), "bar focus indicator: {s:?}");
    }
}
