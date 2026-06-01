//! The persistent bottom bar shown in the grid. Reports the focused box's
//! session + connection + scrollback, the layout, and the prefix-key hints.
//! Survives the agent's alt-screen because ratatui owns the whole screen.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};

use crate::layout::Layout;
use crate::pane::{ConnState, Pane};

const BAR_BG: Color = Color::Rgb(15, 23, 32);

#[allow(clippy::too_many_arguments)]
pub fn render(
    f: &mut Frame,
    area: Rect,
    focused: Option<&Pane>,
    layout: Layout,
    active: usize,
    count: usize,
    prefix_label: &str,
    prefix_armed: bool,
) {
    f.render_widget(Block::default().style(Style::default().bg(BAR_BG)), area);

    let mut left: Vec<Span> = Vec::new();
    match focused {
        Some(p) => {
            let (cols, rows) = p.size();
            let (state_txt, state_col) = match p.state() {
                ConnState::Connecting => ("connecting…", Color::Yellow),
                ConnState::Open => ("● live", Color::Green),
                ConnState::Closed => ("✕ closed", Color::Red),
            };
            left.push(Span::styled(
                format!(" {} ", p.session),
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            left.push(Span::styled(format!("  {state_txt}"), Style::default().fg(state_col)));
            left.push(Span::styled(format!("  {cols}×{rows}"), Style::default().fg(Color::DarkGray)));
            if p.scroll_offset() > 0 {
                left.push(Span::styled(
                    format!("  ⇡ scrollback {}", p.scroll_offset()),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
        }
        None => {
            left.push(Span::styled(" empty box ", Style::default().fg(Color::Black).bg(Color::DarkGray)));
            left.push(Span::styled("  ⏎ to pick a session", Style::default().fg(Color::DarkGray)));
        }
    }
    if count > 1 {
        left.push(Span::styled(
            format!("   {} · box {}/{}", layout.label(), active + 1, count),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if prefix_armed {
        left.push(Span::styled(
            format!("  {prefix_label}-"),
            Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);

    let hint = if count > 1 {
        format!("{prefix_label} ←/→ focus · l layout · d detach  ")
    } else {
        format!("{prefix_label} l layout · d detach  ")
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))))
            .alignment(Alignment::Right),
        area,
    );
}
