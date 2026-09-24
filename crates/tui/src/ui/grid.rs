//! The tiled grid view: each box is a session's emulator, bordered with its
//! name; the focused box gets an accent border. `Single` keeps the clean
//! borderless full-screen look. A shared bottom bar shows the focused box.

use cc_screen_protocol::SessionInfo;
use ratatui::{
    layout::{Alignment, Constraint, Layout as RLayout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::layout::{bordered, tiles, Layout};
use crate::pane::Pane;
use crate::ui::statusbar;
use crate::ui::util::{name_with_crumb, session_crumb};

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

/// A pane's box title, name-first (web parity, proposal 0033): the session's
/// display name — the operator label (0059 C1) resolved by identity from the
/// live list, else the pane's session name — in the title style, then dim
/// context: the machine when aggregated through a hub, and the folder
/// breadcrumb (0025). Fits `max` columns; the context gives way before the
/// name does, and the folder is cut from the left so its leaf survives.
fn pane_title(p: &Pane, sessions: &[SessionInfo], max: usize, ts: Style) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let info = sessions.iter().find(|s| s.name == p.session && s.machine == p.machine);
    let name = info.map(crate::app::display_name).unwrap_or(p.session.as_str());
    let crumb = info.and_then(|s| session_crumb(&s.cwd));
    let context = match (p.machine.is_empty(), crumb) {
        (true, None) => None,
        (true, Some(c)) => Some(c),
        (false, None) => Some(p.machine.clone()),
        (false, Some(c)) => Some(format!("{}:{c}", p.machine)),
    };
    // " name · context " — the padding spaces are part of the budget.
    let inner = max.saturating_sub(2);
    let mut spans = name_with_crumb(
        name,
        context.as_deref(),
        inner,
        ts,
        Style::default().fg(Color::DarkGray),
    );
    // `name_with_crumb` pads to a fixed column; a border title wants none.
    if let Some(last) = spans.last() {
        if !last.content.is_empty() && last.content.trim().is_empty() {
            spans.pop();
        }
    }
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    let mut line = vec![Span::raw(" ")];
    line.extend(spans);
    if used > 0 {
        line.push(Span::raw(" "));
    }
    Line::from(line)
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
    // The title may use the whole top edge bar the two corners.
    let max = rect.width.saturating_sub(2) as usize;
    let title = match pane {
        Some(p) => pane_title(p, sessions, max, ts),
        None => Line::from(Span::styled(" empty ", ts)),
    };
    let block = Block::default().borders(Borders::ALL).border_style(bs).title(title);
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
    async fn box_title_and_bar_name_the_session_and_its_folder() {
        let mut info: SessionInfo = serde_json::from_value(serde_json::json!({
            "name": "cc-api", "tool": "claude", "short": "api", "attached": true,
            "activity": 0, "preview": "", "cwd": "/home/u/dev/api-server",
            "label": "Billing fix"
        }))
        .unwrap();
        info.machine = String::new();
        let panes = vec![Some(dummy(1, "cc-api")), None];
        let sessions = vec![info];
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        t.draw(|f| render(f, Layout::Stacked, &panes, &sessions, 0, "^A", false, false, None, None))
            .unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains(" Billing fix · dev/api-server "), "box title: {s:?}");

        // Single has no border, so the bar is where the folder shows.
        let one = vec![Some(dummy(1, "cc-api"))];
        let mut t = Terminal::new(TestBackend::new(100, 10)).unwrap();
        t.draw(|f| render(f, Layout::Single, &one, &sessions, 0, "^A", false, false, None, None))
            .unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains(" Billing fix   dev/api-server  connecting"), "bar name + folder: {s:?}");

        // Narrow: the folder gives way, the name stays.
        let mut t = Terminal::new(TestBackend::new(24, 12)).unwrap();
        t.draw(|f| render(f, Layout::Stacked, &panes, &sessions, 0, "^A", false, false, None, None))
            .unwrap();
        let s: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(s.contains("Billing fix"), "name survives a narrow box: {s:?}");
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
