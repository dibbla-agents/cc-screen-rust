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

use cc_screen_protocol::SessionInfo;

use crate::layout::Layout;
use crate::pane::{ConnState, Pane};

const BAR_BG: Color = Color::Rgb(15, 23, 32);
/// Amber accent for the ready-session toast (0018 §3), matching the web toast.
const TOAST_BG: Color = Color::Rgb(245, 158, 11);

#[allow(clippy::too_many_arguments)]
pub fn render(
    f: &mut Frame,
    area: Rect,
    focused: Option<&Pane>,
    // The live session list, so the focused box's name reflects an operator rename
    // (0059 C1) — resolved by identity, falling back to the pane's session name.
    sessions: &[SessionInfo],
    layout: Layout,
    active: usize,
    count: usize,
    prefix_label: &str,
    prefix_armed: bool,
    // Keyboard-scroll mode on the focused pane (0059 C3). When set, the
    // scrollback indicator shows even at offset 0 (marking the mode) and the
    // right segment lists the scroll keys.
    scroll_mode: bool,
    // The ready-session toast (0018 §3); when present it takes the bar's
    // right-aligned segment in place of the key hints.
    toast: Option<&str>,
    // A transient note (0069 Part D: `^A [` refused in the alt screen). Same
    // segment as the toast, one rank below it — a ready session matters more.
    hint: Option<&str>,
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
            let name = sessions
                .iter()
                .find(|s| s.name == p.session && s.machine == p.machine)
                .map(crate::app::display_name)
                .unwrap_or(p.session.as_str());
            left.push(Span::styled(
                format!(" {name} "),
                Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
            ));
            left.push(Span::styled(format!("  {state_txt}"), Style::default().fg(state_col)));
            left.push(Span::styled(format!("  {cols}×{rows}"), Style::default().fg(Color::DarkGray)));
            if scroll_mode || p.scroll_offset() > 0 {
                left.push(Span::styled(
                    format!("  ⇡ scrollback {}", p.scroll_offset()),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
            // The child owns the whole screen (0069): its own view, no scrollback
            // here, and the wheel goes to it — worth one glyph so the state is
            // legible rather than surprising.
            if p.alt_screen() {
                left.push(Span::styled("  ⛶ app view", Style::default().fg(Color::DarkGray)));
            }
        }
        None => {
            left.push(Span::styled(" empty box ", Style::default().fg(Color::Black).bg(Color::DarkGray)));
            left.push(Span::styled("  ⏎ for menu", Style::default().fg(Color::DarkGray)));
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

    // The right segment is the ready-session toast when one is up (0018 §3),
    // otherwise the usual key hints. The toast is non-modal — it lives in the
    // app-owned bar and never occludes a pane.
    let right = match toast {
        Some(t) => Line::from(Span::styled(
            t.to_string(),
            Style::default().fg(Color::Black).bg(TOAST_BG).add_modifier(Modifier::BOLD),
        )),
        // A hint is advice, not an event: plain yellow text, no toast chip.
        None if hint.is_some() => Line::from(Span::styled(
            format!("{}  ", hint.unwrap_or_default()),
            Style::default().fg(Color::Yellow),
        )),
        None if scroll_mode => Line::from(Span::styled(
            "PgUp/PgDn ^U/^D · k/j · g/G · q live  ".to_string(),
            Style::default().fg(Color::Yellow),
        )),
        None => {
            let hint = if count > 1 {
                format!("{prefix_label} ←/→ focus · l layout · d menu · s switcher  ")
            } else {
                format!("{prefix_label} l layout · d menu · s switcher  ")
            };
            Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))
        }
    };
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}
