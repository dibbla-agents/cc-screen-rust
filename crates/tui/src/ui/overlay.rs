//! Centered modal overlays for the switcher: a kill/exit confirm and the
//! new-session form.

use cc_screen_protocol::{RestorableSession, SessionInfo};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{FormField, MenuItem, MenuRow};
use crate::client::DirEntry;
use crate::ui::util::{dir_crumb, truncate, truncate_w};

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

/// What the rename overlay (0059 C1) needs to render.
pub struct RenameView<'a> {
    /// The session's identity name, shown read-only for context.
    pub current: &'a str,
    /// The edited display label (empty = will clear the label → slug fallback).
    pub value: &'a str,
    pub error: Option<&'a str>,
}

/// The rename-session overlay: a read-only identity line + one free-text field
/// seeded with the current display name, mirroring the new-session text-field look.
pub fn rename_session(f: &mut Frame, v: &RenameView) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" session  ", dim),
            Span::styled(v.current.to_string(), Style::default().fg(Color::Gray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" name  ", dim),
            Span::raw(v.value.to_string()),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
    ];
    if let Some(e) = v.error {
        lines.push(Line::from(Span::styled(format!(" {e}"), Style::default().fg(Color::Red))));
    }
    lines.push(Line::from(Span::styled(
        " enter save · empty clears label · esc cancel",
        dim,
    )));
    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), 60, h), " rename session ");
    f.render_widget(Paragraph::new(lines), inner);
}

/// What the restorable-session picker (0059 C5) needs to render. `selected` is
/// parallel to `items` (per-row checkbox); `cursor` is the highlighted row.
pub struct RestoreView<'a> {
    pub items: &'a [RestorableSession],
    pub selected: &'a [bool],
    pub cursor: usize,
}

/// The restorable-session picker: a checklist of the sessions a redeploy left
/// recorded-but-not-running. Toggles are a preview of what restore-all brings back
/// (restore is all-or-nothing server-side — see `App::submit_restore_picker`).
pub fn restore_picker(f: &mut Frame, v: &RestoreView) {
    let accent = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let gray = Style::default().fg(Color::Gray);

    let mut lines: Vec<Line> = Vec::new();
    for (i, item) in v.items.iter().enumerate() {
        let cur = i == v.cursor;
        let checked = v.selected.get(i).copied().unwrap_or(false);
        let marker = if cur { "▸" } else { " " };
        let check = if checked { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{check} "), if checked { accent } else { dim }),
            Span::styled(
                format!("{:<20} ", truncate(&item.short, 20)),
                if cur { accent } else { gray },
            ),
            Span::styled(format!("{:<8} ", truncate(&item.tool, 8)), Style::default().fg(Color::Cyan)),
            Span::styled(truncate(&item.dir, 26), dim),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " space toggle · a all · enter restore · esc cancel",
        dim,
    )));

    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), 68, h), " restore sessions ");
    f.render_widget(Paragraph::new(lines), inner);
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
    /// Per-session launch policy toggle (0005). Default: YOLO on. (0014 retired
    /// the hub-control toggle that sat beside it.)
    pub skip_permissions: bool,
    /// The assistant's own remote-control switch (0082). Default: off.
    pub assistant_remote_control: bool,
    /// Whether the selected tool has a remote control to turn on at all — the
    /// row is hidden otherwise, so no switch is offered that changes nothing.
    pub remote_control_available: bool,
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

    // A policy-toggle row: `label  [value]  description`, the bracketed value
    // colored when "on" (the more-exposed state) and dimmed when "off".
    let toggle_line =
        |label: &str, on: bool, focused: bool, on_txt: &str, off_txt: &str, on_color: Color, desc: &str| {
            let marker = if focused { "▸" } else { " " };
            let (txt, col) = if on { (on_txt, on_color) } else { (off_txt, Color::DarkGray) };
            Line::from(vec![
                Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{label:<6}"), dim),
                Span::styled(
                    format!("[{txt}]"),
                    Style::default().fg(col).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {desc}"), dim),
            ])
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

    // Per-session launch policy (0005). Pre-filled to the right default; the
    // "on" state is the more-exposed choice, so it carries the color.
    lines.push(toggle_line(
        "perms",
        v.skip_permissions,
        v.focus == FormField::SkipPermissions,
        "YOLO",
        "ask",
        Color::Yellow,
        "skip permission prompts",
    ));

    // The assistant's own remote control (0082) — off by default, and shown only
    // where flipping it does something. "On" is the outward-facing state, so it
    // carries the colour, exactly like the permissions row above.
    if v.remote_control_available {
        lines.push(toggle_line(
            "claude",
            v.assistant_remote_control,
            v.focus == FormField::AssistantRemoteControl,
            "app",
            "off",
            Color::Magenta,
            "register with the Claude app",
        ));
    }

    lines.push(Line::from(""));
    if let Some(e) = v.error {
        lines.push(Line::from(Span::styled(format!(" {e}"), Style::default().fg(Color::Red))));
    }
    let toggle_focused =
        matches!(v.focus, FormField::SkipPermissions | FormField::AssistantRemoteControl);
    let hint = if dir_focused {
        " ↑↓ pick · tab/→ open · enter create · esc cancel"
    } else if toggle_focused {
        " space/←→ toggle · tab field · enter create · esc cancel"
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

/// What the unified grid action menu needs to render (0062b, search-first).
/// `rows` is the precomputed display list (`App::menu_rows` — resting order or
/// filtered+ranked); `selected_row` is the ROW index of the selected selectable
/// row (headers already skipped by the caller). The overlay stays dumb: no
/// scoring, no App access.
pub struct MenuView<'a> {
    pub rows: &'a [MenuRow],
    pub sessions: &'a [SessionInfo],
    /// The menu's type-to-search query (rendered in the query line).
    pub query: &'a str,
    pub selected_row: Option<usize>,
    /// Per-session machine chip text, parallel to `sessions`. Empty slice (or
    /// an empty string) = no chip; non-empty only while filtering in
    /// multi-machine hub mode (headers cover the resting state).
    pub chips: &'a [String],
    pub box_num: usize,
    pub box_count: usize,
}

fn menu_marker(sel: bool) -> Span<'static> {
    Span::styled(
        format!("{} ", if sel { "▸" } else { " " }),
        Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
    )
}

/// The action rows' glyph + label, shared by the resting and filtered lists.
fn menu_action_face(item: MenuItem) -> (&'static str, &'static str) {
    match item {
        MenuItem::ChangeLayout => ("▦", "Change layout"),
        MenuItem::NewSession => ("✚", "New session"),
        MenuItem::RenameSession => ("✎", "Rename session"),
        MenuItem::ClearBox => ("✕", "Clear this box"),
        MenuItem::Quit => ("⏻", "Quit ccs"),
    }
}

/// The unified grid action menu — a centered list over the grid (so the boxes
/// stay visible), search-first per 0062b. At rest: Change layout / New session
/// above the (machine-grouped) session list, Rename / Clear this box / Quit
/// below. Typing filters: the whole ranked list becomes one windowed region.
/// Reached via `Ctrl-A d` or Enter/click on an empty box.
pub fn grid_menu(f: &mut Frame, v: &MenuView) {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    const MAX_SESS: usize = 8;
    let sel_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(Color::DarkGray);
    let gray = Style::default().fg(Color::Gray);

    // ~75% of the terminal width (the box is the focus while it's open), clamped
    // to something usable on tiny terminals and never wider than the screen.
    let area_w = f.area().width;
    let w = (area_w as u32 * 3 / 4).max(40).min(area_w as u32) as u16;
    let inner_w = w.saturating_sub(2) as usize; // panel borders take one col each side
    let inner_h = f.area().height.saturating_sub(2) as usize; // …and one row each

    // A divider that spans the inner width (inset one column each side).
    let sep = || Line::from(Span::styled(format!(" {} ", "─".repeat(inner_w.saturating_sub(2))), dim));

    // The session rows' display name: the operator label (0059 C1) when set,
    // else the folder breadcrumb — web `displayName` parity with the switcher.
    let named = |s: &SessionInfo| match s.label.as_deref() {
        Some(l) if !l.is_empty() => l.to_string(),
        _ => dir_crumb(&s.cwd, &s.name),
    };

    // Size the name column to the longest display name so names show in full;
    // the tool column is fixed and the trailing summary takes whatever's left
    // (it's the croppable one). lead = marker (2) + attached dot (2) + ready
    // marker (2); the two inter-column gaps add 2 more. All column math is in
    // display COLUMNS (unicode-width), not chars — a double-width CJK name
    // would otherwise misalign the tool column and overrun the border.
    const TOOL_W: usize = 7;
    let lead = 6usize;
    let longest = v.sessions.iter().map(|s| named(s).width()).max().unwrap_or(0);
    let name_cap = inner_w.saturating_sub(lead + 2 + TOOL_W + 8).max(12); // keep ≥8 for the tail
    let name_w = longest.clamp(12, name_cap);
    let preview_w = inner_w.saturating_sub(lead + name_w + 2 + TOOL_W);

    // Render one display row; `ri` is its index into `v.rows` (what
    // `selected_row` addresses).
    let render_row = |ri: usize, row: &MenuRow| -> Line<'static> {
        let sel = v.selected_row == Some(ri);
        match row {
            MenuRow::Action(item) => {
                let (glyph, label) = menu_action_face(*item);
                Line::from(vec![
                    menu_marker(sel),
                    Span::styled(format!("{glyph}  {label}"), if sel { sel_style } else { gray }),
                ])
            }
            // A per-machine group header (resting multi-machine hub mode) —
            // non-selectable, styled like the switcher's.
            MenuRow::Header { label, online } => {
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{} ", truncate_w(label, inner_w.saturating_sub(14).max(4))),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                    ),
                ];
                if !online {
                    spans.push(Span::styled(
                        "· offline",
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
                    ));
                }
                Line::from(spans)
            }
            MenuRow::Session(i) => {
                let s = &v.sessions[*i];
                let dot = if s.attached { "●" } else { "○" };
                // A ready/working indicator (0018 §6), mirroring the switcher:
                // an amber ● marks a session still producing output, absent
                // once it's waiting (ready). So the durable menu agrees with
                // the toast about who is ready — not just who is attached.
                let work = if s.waiting { "  " } else { "● " };
                let mut spans = vec![
                    menu_marker(sel),
                    Span::styled(
                        format!("{dot} "),
                        Style::default().fg(if sel { Color::Cyan } else { Color::DarkGray }),
                    ),
                    Span::styled(
                        work,
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    ),
                    {
                        // Width-aware padding: `{:<name_w$}` pads by chars, so a
                        // CJK name would leave the column ~2x too wide.
                        let name = truncate_w(&named(s), name_w);
                        let pad = " ".repeat(name_w.saturating_sub(name.width()));
                        Span::styled(
                            format!("{name}{pad} "),
                            if sel { sel_style } else { gray },
                        )
                    },
                    Span::styled(format!("{:<TOOL_W$} ", truncate(&s.tool, TOOL_W)), dim),
                ];
                let mut avail = preview_w;
                // The inline machine chip (filtering, multi-machine): the group
                // headers are suppressed, so the row itself says where the
                // session lives. The chip must FIT the tail budget — clamp it
                // (12 stays the aesthetic cap) and skip it entirely when there
                // isn't room for "[x] ", else the row overflows and gets
                // hard-clipped at the panel border (bracket and summary lost).
                let chip = v.chips.get(*i).map(String::as_str).unwrap_or("");
                if !chip.is_empty() && avail >= 6 {
                    let host = truncate_w(chip, avail.saturating_sub(3).min(12));
                    avail = avail.saturating_sub(host.width() + 3);
                    spans.push(Span::styled(format!("[{host}] "), dim));
                }
                // Trailing summary (0062 Part C, web-parity emphasis): the LLM
                // headline in the summary grey; the raw preview fallback
                // visibly raw — darker + italic; neither → "—".
                if avail > 1 {
                    let (tail, style) = match s.headline.as_deref() {
                        Some(h) if !h.is_empty() => (h, Style::default().fg(Color::Gray)),
                        _ if !s.preview.is_empty() => (
                            s.preview.as_str(),
                            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                        ),
                        _ => ("—", Style::default().fg(Color::DarkGray)),
                    };
                    spans.push(Span::styled(truncate_w(tail, avail), style));
                }
                Line::from(spans)
            }
        }
    };

    // Window `region` (rows starting at absolute index `base`) to `cap` rows,
    // keeping the cursor in view — the MAX_SESS windowing generalized to any
    // contiguous region (the middle at rest, the whole list while filtering).
    // `sel_line` records the pushed line index of the selected row so the
    // final render can scroll it into view on a too-short terminal.
    let windowed = |lines: &mut Vec<Line<'static>>,
                    sel_line: &mut Option<usize>,
                    base: usize,
                    region: &[MenuRow],
                    cap: usize| {
        let shown = region.len().min(cap.max(1));
        let sel = v.selected_row.and_then(|r| r.checked_sub(base)).filter(|&i| i < region.len());
        let start = match sel {
            Some(i) => i.saturating_sub(shown.saturating_sub(1)).min(region.len() - shown),
            None => 0,
        };
        for (j, row) in region.iter().enumerate().skip(start).take(shown) {
            if v.selected_row == Some(base + j) {
                *sel_line = Some(lines.len());
            }
            lines.push(render_row(base + j, row));
        }
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line: Option<usize> = None;

    // The query line (mirrors the switcher's `render_query`): the always-visible
    // search box, truncating from the LEFT so the tail stays visible.
    let mut qspans = vec![Span::styled(" › ", Style::default().fg(Color::Cyan))];
    if v.query.is_empty() {
        qspans.push(Span::styled(
            "type to search",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ));
    } else {
        // Left-truncate by display columns (not chars), so a wide char in the
        // query can't push the cursor glyph past the border.
        let avail = inner_w.saturating_sub(5).max(1); // prefix + cursor
        let chars: Vec<char> = v.query.chars().collect();
        let mut qw: usize = chars.iter().map(|c| c.width().unwrap_or(0)).sum();
        let mut start = 0usize;
        while qw > avail && start < chars.len() {
            qw -= chars[start].width().unwrap_or(0);
            start += 1;
        }
        let shown: String = chars[start..].iter().collect();
        qspans.push(Span::styled(shown, Style::default().add_modifier(Modifier::BOLD)));
        qspans.push(Span::styled("▏", Style::default().fg(Color::Cyan)));
    }
    lines.push(Line::from(qspans));

    if v.query.trim().is_empty() {
        // Resting: the two create-ish actions pinned on top, the (windowed)
        // session/header region between separators, the tail actions pinned
        // below — `App::menu_rows` guarantees this shape.
        let top = 2.min(v.rows.len());
        let bottom_start = v
            .rows
            .iter()
            .rposition(|r| !matches!(r, MenuRow::Action(_)))
            .map(|i| i + 1)
            .unwrap_or(top);
        // The mid window gets whatever height the fixed chrome (query line,
        // pinned action groups, separators, footer) leaves — capped at
        // MAX_SESS for aesthetics — so a short terminal shrinks the session
        // region instead of clipping the tail actions and the hint off the
        // panel bottom.
        let chrome = 1 + top + 2 + (v.rows.len() - bottom_start) + 2;
        let cap = inner_h.saturating_sub(chrome).clamp(1, MAX_SESS);
        for (ri, row) in v.rows.iter().enumerate().take(top) {
            if v.selected_row == Some(ri) {
                sel_line = Some(lines.len());
            }
            lines.push(render_row(ri, row));
        }
        lines.push(sep());
        let mid = &v.rows[top..bottom_start];
        if mid.is_empty() {
            lines.push(Line::from(Span::styled("     no sessions", dim)));
        } else {
            windowed(&mut lines, &mut sel_line, top, mid, cap);
        }
        lines.push(sep());
        for (ri, row) in v.rows.iter().enumerate().skip(bottom_start) {
            if v.selected_row == Some(ri) {
                sel_line = Some(lines.len());
            }
            lines.push(render_row(ri, row));
        }
    } else if v.rows.is_empty() {
        lines.push(Line::from(Span::styled("     no matches — esc to clear", dim)));
    } else {
        // Filtering: one flat ranked region (actions and sessions interleaved
        // by score), windowed around the cursor. Height-adaptive like the
        // resting mid region (chrome here is just the query line + footer).
        let cap = inner_h.saturating_sub(3).clamp(1, MAX_SESS);
        windowed(&mut lines, &mut sel_line, 0, v.rows, cap);
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" type to search · ↑↓ · ⏎ select · esc", dim)));

    let h = lines.len() as u16 + 2;
    let inner = panel(f, centered(f.area(), w, h), &format!(" box {}/{} ", v.box_num, v.box_count));
    // Last-resort scroll: `centered` clamps the panel to the terminal, so on a
    // very short screen lines still fall off the bottom — scroll the SELECTED
    // row into view rather than leave an invisible row actionable (Enter
    // resolves through the rows regardless of what's rendered).
    let view_h = inner.height as usize;
    let scroll = match sel_line {
        Some(sl) if view_h > 0 && sl >= view_h => (sl + 1 - view_h) as u16,
        _ => 0,
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
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
            last_input_at: 0,
            busy_since: 0,
            busy_until: 0,
            preview: "p".into(),
            waiting: false,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
            label: None,
            assistant_remote_control: None,
        }
    }

    /// The resting row list `App::menu_rows` builds for `sessions` (no headers,
    /// direct mode, no rename) — the shape the overlay contract relies on.
    fn resting_rows(n: usize) -> Vec<MenuRow> {
        let mut rows = vec![MenuRow::Action(MenuItem::ChangeLayout), MenuRow::Action(MenuItem::NewSession)];
        rows.extend((0..n).map(MenuRow::Session));
        rows.push(MenuRow::Action(MenuItem::ClearBox));
        rows.push(MenuRow::Action(MenuItem::Quit));
        rows
    }

    #[test]
    fn grid_menu_lists_actions_and_sessions() {
        let list = vec![sess("claude-a"), sess("codex-b")];
        let rows = resting_rows(2);
        // selected_row = 2 → the first session row is highlighted.
        let s = render_to(72, 18, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "",
                    selected_row: Some(2),
                    chips: &[],
                    box_num: 2,
                    box_count: 4,
                },
            )
        });
        assert!(s.contains("box 2/4"), "{s}");
        assert!(s.contains("Change layout"), "{s}");
        assert!(s.contains("New session"), "{s}");
        assert!(s.contains("Clear this box"), "{s}");
        assert!(s.contains("Quit ccs"), "{s}");
        assert!(s.contains("claude-a"), "{s}");
        assert!(s.contains("codex-b"), "{s}");
        assert!(s.contains('▸'), "selection marker: {s}");
        assert!(s.contains("type to search"), "query placeholder: {s}");
    }

    #[test]
    fn grid_menu_marks_ready_vs_busy_sessions() {
        // A busy (working) session carries the amber ● marker; a waiting (ready)
        // one doesn't — both rows are detached (○) so the only ● is the worker.
        let mut busy = sess("busy");
        busy.waiting = false;
        let mut ready = sess("ready");
        ready.waiting = true;
        let rows = resting_rows(1);
        fn view<'a>(rows: &'a [MenuRow], list: &'a [SessionInfo]) -> MenuView<'a> {
            MenuView {
                rows,
                sessions: list,
                query: "",
                selected_row: Some(1),
                chips: &[],
                box_num: 1,
                box_count: 1,
            }
        }
        let busy = [busy];
        let s = render_to(72, 16, |f| grid_menu(f, &view(&rows, &busy)));
        assert!(s.contains('●'), "busy row should show the working marker: {s}");
        let ready = [ready];
        let s = render_to(72, 16, |f| grid_menu(f, &view(&rows, &ready)));
        assert!(!s.contains('●'), "ready/waiting row should not show the working marker: {s}");
    }

    #[test]
    fn grid_menu_empty_still_shows_actions() {
        let rows = resting_rows(0);
        let s = render_to(72, 14, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &[],
                    query: "",
                    selected_row: Some(1),
                    chips: &[],
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("no sessions"), "{s}");
        assert!(s.contains("New session"), "{s}");
        assert!(s.contains("Quit ccs"), "{s}");
    }

    #[test]
    fn grid_menu_filtering_renders_ranked_rows_query_and_chip() {
        // Filtering (0062b): the query line shows the typed text, the ranked
        // rows interleave sessions and actions with no separators pinning, the
        // session name uses the label/dir-crumb naming, and the machine chip
        // rides the row.
        let mut s0 = sess("claude-a");
        s0.label = Some("My Feature".into());
        let list = [s0];
        let rows = vec![MenuRow::Session(0), MenuRow::Action(MenuItem::NewSession)];
        let chips = vec!["hostA".to_string()];
        let s = render_to(80, 14, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "fea",
                    selected_row: Some(0),
                    chips: &chips,
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("› fea"), "typed query missing: {s}");
        assert!(s.contains("My Feature"), "label naming missing: {s}");
        assert!(s.contains("[hostA]"), "machine chip missing: {s}");
        assert!(s.contains("New session"), "ranked action row missing: {s}");
        assert!(!s.contains("Change layout"), "dropped rows must not render: {s}");
    }

    #[test]
    fn grid_menu_short_terminal_keeps_the_selection_visible() {
        // 40x10, 8 sessions, cursor on Quit (the pinned tail): the mid window
        // shrinks to the height the chrome leaves, and the panel scrolls the
        // selected row into view — an invisible row would still be actionable.
        let list: Vec<_> = (0..8).map(|i| sess(&format!("sess-{i}"))).collect();
        let rows = resting_rows(8);
        let s = render_to(40, 10, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "",
                    selected_row: Some(rows.len() - 1),
                    chips: &[],
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("Quit ccs"), "selected tail action visible: {s}");
        assert!(s.contains('▸'), "selection marker visible: {s}");
        // Even shorter (30x6): the scroll fallback still brings Quit in view.
        let s = render_to(30, 6, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "",
                    selected_row: Some(rows.len() - 1),
                    chips: &[],
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("Quit ccs"), "scrolled into view: {s}");
        assert!(s.contains('▸'), "selection marker visible: {s}");
    }

    #[test]
    fn grid_menu_chip_clamps_to_the_tail_budget() {
        // 40 cols with a long name leaves an 8-column tail; the fixed 12-char
        // chip cap would overflow past the border (losing its bracket), so the
        // chip clamps to what fits.
        let list = [sess("a-very-long-session-name")];
        let rows = vec![MenuRow::Session(0)];
        let chips = vec!["mac-studio-ubuntu".to_string()];
        let s = render_to(40, 12, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "q",
                    selected_row: Some(0),
                    chips: &chips,
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("[mac-…]"), "clamped chip keeps its closing bracket: {s}");
    }

    #[test]
    fn grid_menu_zero_matches_shows_hint() {
        let list = [sess("claude-a")];
        let s = render_to(72, 12, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &[],
                    sessions: &list,
                    query: "zzz",
                    selected_row: None,
                    chips: &[],
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("no matches"), "{s}");
        assert!(!s.contains("Quit ccs"), "dropped actions must not render: {s}");
    }

    #[test]
    fn grid_menu_headers_render_like_the_switcher() {
        // Resting multi-machine hub mode: non-selectable hostname headers with
        // the offline marker, between the pinned action groups.
        let list = [sess("claude-a")];
        let rows = vec![
            MenuRow::Action(MenuItem::ChangeLayout),
            MenuRow::Action(MenuItem::NewSession),
            MenuRow::Header { label: "hostB".into(), online: false },
            MenuRow::Session(0),
            MenuRow::Action(MenuItem::ClearBox),
            MenuRow::Action(MenuItem::Quit),
        ];
        let s = render_to(72, 16, |f| {
            grid_menu(
                f,
                &MenuView {
                    rows: &rows,
                    sessions: &list,
                    query: "",
                    selected_row: Some(3),
                    chips: &[],
                    box_num: 1,
                    box_count: 1,
                },
            )
        });
        assert!(s.contains("hostB"), "header missing: {s}");
        assert!(s.contains("offline"), "offline marker missing: {s}");
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
            skip_permissions: true,
            assistant_remote_control: false,
            remote_control_available: true,
        };
        let s = render_to(70, 14, |f| new_session(f, &v));
        assert!(s.contains("new session"), "{s}");
        assert!(s.contains("claude"), "{s}");
        assert!(s.contains("proj"), "{s}");
        assert!(s.contains("/home/u"), "{s}");
        assert!(s.contains("already exists"), "{s}");
        // No machine → the machine row is absent.
        assert!(!s.contains("machine"), "{s}");
        // The skip-permissions toggle renders with its default (YOLO on). The
        // hub-control toggle was retired by 0014 — no "view-only" affordance.
        assert!(s.contains("YOLO"), "skip-permissions toggle: {s}");
        assert!(!s.contains("view-only"), "no retired hub-control toggle: {s}");
    }

    #[test]
    fn new_session_remote_control_row_is_capability_gated() {
        // 0082: the row renders — off by default — for a capable tool…
        let mut v = NewSessionView {
            tool: "claude",
            machine: None,
            machine_online: true,
            machine_pickable: true,
            name: "proj",
            dir: "/home/u",
            focus: FormField::AssistantRemoteControl,
            candidates: &[],
            cand_sel: None,
            error: None,
            skip_permissions: true,
            assistant_remote_control: false,
            remote_control_available: true,
        };
        let s = render_to(70, 16, |f| new_session(f, &v));
        assert!(s.contains("register with the Claude app"), "{s}");
        assert!(s.contains("[off]"), "default is off: {s}");
        // …reads "app" when on…
        v.assistant_remote_control = true;
        let s = render_to(70, 16, |f| new_session(f, &v));
        assert!(s.contains("[app]"), "{s}");
        // …and is absent entirely for a tool with nothing to turn on.
        v.remote_control_available = false;
        let s = render_to(70, 16, |f| new_session(f, &v));
        assert!(!s.contains("register with the Claude app"), "{s}");
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
            skip_permissions: true,
            assistant_remote_control: false,
            remote_control_available: true,
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
            skip_permissions: true,
            assistant_remote_control: false,
            remote_control_available: true,
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
            skip_permissions: true,
            assistant_remote_control: false,
            remote_control_available: true,
        };
        let s = render_to(70, 18, |f| new_session(f, &v));
        assert!(s.contains("dev"), "{s}");
        assert!(s.contains("docs"), "{s}");
        assert!(s.contains('›'), "selection marker: {s}");
    }
}
