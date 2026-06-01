//! Top-level app state + a single unified event loop. All inputs — terminal
//! events, the 1 s poll tick, pane WebSocket bytes, and async action results —
//! funnel into one `mpsc<AppMsg>` channel. Two modes: the session switcher
//! (with modal overlays) and the tiled grid of attached boxes.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use cc_screen_protocol::{CreateReq, SessionInfo, ToolInfo};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::client::{ws, Rest};
use crate::config::Config;
use crate::input;
use crate::layout::{self, Layout};
use crate::pane::{ConnState, Pane, WsOut};
use crate::term::Tui;
use crate::ui;

/// Everything the event loop reacts to.
pub enum AppMsg {
    Term(Event),
    Tick,
    /// Bytes/state from a pane's WS task, tagged with the pane's id.
    Pane { id: u64, msg: PaneMsg },
    /// Result of an async create: Ok(session name) or Err(message).
    Created(Result<String, String>),
}

pub enum PaneMsg {
    Bytes(Vec<u8>),
    State(ConnState),
}

enum Mode {
    Switcher,
    Grid,
}

/// A modal over the grid.
#[derive(Clone, Copy)]
enum GridOverlay {
    None,
    Palette(usize),                          // highlighted index in Layout::ALL
    Pick { target: usize, selected: usize }, // session picker for box `target`
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A modal over the switcher.
enum Overlay {
    None,
    Confirm { session: String, graceful: bool },
    NewSession(NewForm),
}

struct NewForm {
    tool_idx: usize,
    field: u8, // 0 = name, 1 = dir
    name: String,
    dir: String,
    error: Option<String>,
}

const MOUSE_STEP: isize = 3;

pub struct App {
    rest: Rest,
    cfg: Config,
    tools: Vec<ToolInfo>,
    home: String,
    sessions: Vec<SessionInfo>,
    selected: usize,
    status: String,
    mode: Mode,
    overlay: Overlay,

    // grid
    layout: Layout,
    panes: Vec<Option<Pane>>, // length == layout.count()
    active: usize,            // focused box
    /// When the switcher is opened to fill a specific box, which one.
    fill_target: Option<usize>,
    next_pane_id: u64,
    /// A modal over the grid (layout palette / session picker).
    grid_overlay: GridOverlay,

    area: (u16, u16),
    prefix: (KeyCode, KeyModifiers),
    prefix_armed: bool,
    tx: mpsc::Sender<AppMsg>,
    rx: Option<mpsc::Receiver<AppMsg>>,
    should_quit: bool,
    pending_refresh: bool,
}

impl App {
    pub fn new(rest: Rest, cfg: Config) -> Self {
        let (tx, rx) = mpsc::channel(512);
        let prefix = input::parse_prefix(&cfg.prefix);
        Self {
            rest,
            cfg,
            tools: Vec::new(),
            home: String::new(),
            sessions: Vec::new(),
            selected: 0,
            status: "connecting…".into(),
            mode: Mode::Switcher,
            overlay: Overlay::None,
            layout: Layout::Single,
            panes: vec![None],
            active: 0,
            fill_target: None,
            next_pane_id: 0,
            grid_overlay: GridOverlay::None,
            area: (80, 24),
            prefix,
            prefix_armed: false,
            tx,
            rx: Some(rx),
            should_quit: false,
            pending_refresh: false,
        }
    }

    pub async fn run(mut self, term: &mut Tui) -> Result<()> {
        let mut rx = self.rx.take().expect("run() called once");
        self.spawn_term_events();
        self.spawn_ticker();

        self.tools = self.rest.tools().await.unwrap_or_default();
        self.home = self.rest.home().await.unwrap_or_default();

        self.sync_area(term);
        self.refresh().await;
        term.draw(|f| self.render(f))?;

        while let Some(msg) = rx.recv().await {
            self.handle(msg).await;
            while let Ok(m) = rx.try_recv() {
                self.handle(m).await;
            }
            if self.should_quit {
                break;
            }
            self.sync_area(term);
            self.relayout();
            term.draw(|f| self.render(f))?;
        }
        Ok(())
    }

    fn spawn_term_events(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut events = EventStream::new();
            // Keep reading across transient parse errors — a single bad/partial
            // sequence must NOT make the whole TUI go deaf to input.
            while let Some(res) = events.next().await {
                match res {
                    Ok(ev) => {
                        if tx.send(AppMsg::Term(ev)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => continue,
                }
            }
        });
    }

    fn spawn_ticker(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut poll = tokio::time::interval(Duration::from_secs(1));
            poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                poll.tick().await;
                if tx.send(AppMsg::Tick).await.is_err() {
                    break;
                }
            }
        });
    }

    fn sync_area(&mut self, term: &Tui) {
        if let Ok(sz) = term.size() {
            self.area = (sz.width, sz.height);
        }
    }

    async fn handle(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Tick => self.refresh().await,
            AppMsg::Term(ev) => self.handle_term(ev),
            AppMsg::Pane { id, msg } => self.handle_pane(id, msg),
            AppMsg::Created(res) => self.handle_created(res),
        }
        if self.pending_refresh {
            self.pending_refresh = false;
            self.refresh().await;
        }
    }

    // ── session list ─────────────────────────────────────────────────────────
    async fn refresh(&mut self) {
        match self.rest.sessions().await {
            Ok(mut list) => {
                list.sort_by(|a, b| a.name.cmp(&b.name));
                self.sessions = list;
                if self.selected >= self.sessions.len() {
                    self.selected = self.sessions.len().saturating_sub(1);
                }
                // Auto-detach any box whose session ended.
                let live: HashSet<&str> = self.sessions.iter().map(|s| s.name.as_str()).collect();
                let mut changed = false;
                for slot in self.panes.iter_mut() {
                    if slot.as_ref().is_some_and(|p| !live.contains(p.session.as_str())) {
                        *slot = None;
                        changed = true;
                    }
                }
                if changed {
                    self.after_box_removed();
                }
                if matches!(self.mode, Mode::Switcher) {
                    self.status =
                        format!("{} session(s) · {}", self.sessions.len(), self.rest.urls().base());
                }
            }
            Err(e) => {
                if matches!(self.mode, Mode::Switcher) {
                    self.status = format!("server unreachable — retrying · {}", short_err(&e));
                }
            }
        }
    }

    fn handle_created(&mut self, res: Result<String, String>) {
        match res {
            Ok(name) => {
                self.overlay = Overlay::None;
                // If the create was launched to fill a box, drop it in there;
                // otherwise it was a plain switcher create.
                if let Some(target) = self.fill_target.take() {
                    self.fill_box(target, name);
                } else {
                    self.status = format!("created {name}");
                }
                self.pending_refresh = true;
            }
            Err(e) => {
                if let Overlay::NewSession(f) = &mut self.overlay {
                    f.error = Some(e);
                }
            }
        }
    }

    // ── events ───────────────────────────────────────────────────────────────
    fn handle_term(&mut self, ev: Event) {
        match ev {
            Event::Resize(w, h) => {
                self.area = (w, h);
                self.relayout();
            }
            Event::Key(k) if k.kind == KeyEventKind::Press => match self.mode {
                Mode::Switcher => self.key_switcher(k),
                Mode::Grid => self.key_grid(k),
            },
            Event::Paste(s) if matches!(self.mode, Mode::Grid) => {
                if let Some(p) = self.panes.get(self.active).and_then(|x| x.as_ref()) {
                    p.send_input(cc_screen_protocol::wrap_bracketed_paste(&s, false));
                }
            }
            Event::Mouse(me) => self.handle_mouse(me),
            _ => {}
        }
    }

    fn handle_mouse(&mut self, me: crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind::{Down, ScrollDown, ScrollUp};
        match self.mode {
            Mode::Grid => {
                if !matches!(self.grid_overlay, GridOverlay::None) {
                    return; // an overlay is up — let it own the screen
                }
                match me.kind {
                    // Scroll the box under the cursor (fall back to the focused one).
                    ScrollUp | ScrollDown => {
                        let idx = self.box_at(me.column, me.row).unwrap_or(self.active);
                        if let Some(p) = self.panes.get_mut(idx).and_then(|x| x.as_mut()) {
                            let d = if matches!(me.kind, ScrollUp) { MOUSE_STEP } else { -MOUSE_STEP };
                            p.scroll(d);
                        }
                    }
                    // Click focuses the box; clicking an empty one opens the picker.
                    Down(_) => {
                        if let Some(idx) = self.box_at(me.column, me.row) {
                            self.active = idx;
                            if self.panes.get(idx).and_then(|x| x.as_ref()).is_none() {
                                self.open_pick(idx);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Mode::Switcher => match me.kind {
                ScrollUp => self.move_sel(-1),
                ScrollDown => self.move_sel(1),
                _ => {}
            },
        }
    }

    // ── switcher keys (dispatch by active overlay) ───────────────────────────
    fn key_switcher(&mut self, k: KeyEvent) {
        let kind = match self.overlay {
            Overlay::None => 0,
            Overlay::Confirm { .. } => 1,
            Overlay::NewSession(_) => 2,
        };
        match kind {
            1 => self.key_confirm(k),
            2 => self.key_newform(k),
            _ => self.key_list(k),
        }
    }

    fn key_list(&mut self, k: KeyEvent) {
        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
                // If picking a session to fill a box, cancel back to the grid.
                if self.fill_target.take().is_some() {
                    self.mode = Mode::Grid;
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.move_sel(1),
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.move_sel(-1),
            (KeyCode::Char('r'), _) => self.pending_refresh = true,
            (KeyCode::Enter, _) => self.attach(),
            (KeyCode::Char('n'), _) => self.open_newform(),
            (KeyCode::Char('x'), _) => self.confirm_delete(false),
            (KeyCode::Char('e'), _) => self.confirm_delete(true),
            (KeyCode::Char('R'), _) => self.restore_all(),
            _ => {}
        }
    }

    fn key_confirm(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Overlay::Confirm { session, graceful } =
                    std::mem::replace(&mut self.overlay, Overlay::None)
                {
                    let mode = if graceful { "exit" } else { "kill" };
                    let rest = self.rest.clone();
                    let target = session.clone();
                    tokio::spawn(async move {
                        let _ = rest.delete(&target, mode).await;
                    });
                    self.status =
                        format!("{} {session}", if graceful { "exiting" } else { "killing" });
                    self.pending_refresh = true;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.overlay = Overlay::None,
            _ => {}
        }
    }

    fn key_newform(&mut self, k: KeyEvent) {
        match (k.code, k.modifiers) {
            (KeyCode::Esc, _) => self.overlay = Overlay::None,
            (KeyCode::Enter, _) => self.submit_newform(),
            (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                if let Overlay::NewSession(f) = &mut self.overlay {
                    f.field ^= 1;
                }
            }
            (KeyCode::Left, _) => self.cycle_tool(-1),
            (KeyCode::Right, _) => self.cycle_tool(1),
            (KeyCode::Backspace, _) => {
                if let Overlay::NewSession(f) = &mut self.overlay {
                    if f.field == 0 {
                        f.name.pop();
                    } else {
                        f.dir.pop();
                    }
                }
            }
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
            {
                if let Overlay::NewSession(f) = &mut self.overlay {
                    if f.field == 0 {
                        f.name.push(c);
                    } else {
                        f.dir.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.sessions.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
    }

    // ── lifecycle (create / kill / restore) ──────────────────────────────────
    fn open_newform(&mut self) {
        if self.tools.is_empty() {
            self.status = "no tools available".into();
            return;
        }
        self.overlay = Overlay::NewSession(NewForm {
            tool_idx: 0,
            field: 0,
            name: String::new(),
            dir: self.home.clone(),
            error: None,
        });
    }

    fn cycle_tool(&mut self, d: isize) {
        let n = self.tools.len();
        if n == 0 {
            return;
        }
        if let Overlay::NewSession(f) = &mut self.overlay {
            f.tool_idx = (f.tool_idx as isize + d).rem_euclid(n as isize) as usize;
        }
    }

    fn submit_newform(&mut self) {
        let req = {
            let Overlay::NewSession(f) = &self.overlay else {
                return;
            };
            let Some(t) = self.tools.get(f.tool_idx) else {
                return;
            };
            CreateReq {
                tool: t.prefix.clone(),
                name: f.name.clone(),
                dir: f.dir.clone(),
                extra_dirs: Vec::new(),
            }
        };
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = rest.create(&req).await.map_err(|e| e.to_string());
            let _ = tx.send(AppMsg::Created(r)).await;
        });
    }

    fn confirm_delete(&mut self, graceful: bool) {
        if let Some(s) = self.sessions.get(self.selected) {
            self.overlay = Overlay::Confirm { session: s.name.clone(), graceful };
        }
    }

    fn restore_all(&mut self) {
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = rest.restore().await;
            let _ = tx.send(AppMsg::Tick).await; // nudge a refresh
        });
        self.status = "restoring…".into();
    }

    // ── grid keys ────────────────────────────────────────────────────────────
    fn key_grid(&mut self, k: KeyEvent) {
        // A grid overlay (palette / picker), when open, captures all keys.
        match self.grid_overlay {
            GridOverlay::None => {}
            GridOverlay::Palette(_) => return self.key_palette(k),
            GridOverlay::Pick { .. } => return self.key_pick(k),
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            if self.is_prefix(k) {
                self.send_key_to_active(k); // prefix prefix → literal prefix
                return;
            }
            match k.code {
                KeyCode::Char('d') => self.detach_focused(),
                KeyCode::Char('x') => self.kill_focused(),
                KeyCode::Char('l') | KeyCode::Char(' ') => self.open_palette(),
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    // Direct power-shortcut (the palette is the visual path).
                    if let Some(l) = Layout::from_digit(c as u8 - b'0') {
                        self.set_layout(l);
                    }
                }
                KeyCode::Left => self.focus_dir(Dir::Left),
                KeyCode::Right => self.focus_dir(Dir::Right),
                KeyCode::Up => self.focus_dir(Dir::Up),
                KeyCode::Down => self.focus_dir(Dir::Down),
                _ => {}
            }
            return;
        }
        if self.is_prefix(k) {
            self.prefix_armed = true;
            return;
        }
        if self.panes.get(self.active).and_then(|x| x.as_ref()).is_some() {
            self.send_key_to_active(k);
        } else if k.code == KeyCode::Enter {
            self.open_pick(self.active); // fill this empty box
        }
    }

    // ── layout palette ───────────────────────────────────────────────────────
    fn open_palette(&mut self) {
        let cur = Layout::ALL.iter().position(|&l| l == self.layout).unwrap_or(0);
        self.grid_overlay = GridOverlay::Palette(cur);
        self.prefix_armed = false;
    }

    fn key_palette(&mut self, k: KeyEvent) {
        let GridOverlay::Palette(hi) = self.grid_overlay else {
            return;
        };
        match k.code {
            KeyCode::Esc => self.grid_overlay = GridOverlay::None,
            KeyCode::Left | KeyCode::Up => self.grid_overlay = GridOverlay::Palette((hi + 5) % 6),
            KeyCode::Right | KeyCode::Down => self.grid_overlay = GridOverlay::Palette((hi + 1) % 6),
            KeyCode::Enter => {
                self.grid_overlay = GridOverlay::None;
                self.set_layout(Layout::ALL[hi]);
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(l) = Layout::from_digit(c as u8 - b'0') {
                    self.grid_overlay = GridOverlay::None; // digit jump-applies
                    self.set_layout(l);
                }
            }
            _ => {}
        }
    }

    // ── scoped session picker (fill a box) ───────────────────────────────────
    fn open_pick(&mut self, target: usize) {
        self.grid_overlay = GridOverlay::Pick { target, selected: 0 };
        self.prefix_armed = false;
    }

    fn key_pick(&mut self, k: KeyEvent) {
        let GridOverlay::Pick { target, selected } = self.grid_overlay else {
            return;
        };
        let n = self.sessions.len();
        match k.code {
            KeyCode::Esc => self.grid_overlay = GridOverlay::None,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.grid_overlay = GridOverlay::Pick { target, selected: (selected + n - 1) % n };
            }
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.grid_overlay = GridOverlay::Pick { target, selected: (selected + 1) % n };
            }
            KeyCode::Enter => {
                if let Some(s) = self.sessions.get(selected) {
                    let name = s.name.clone();
                    self.grid_overlay = GridOverlay::None;
                    self.fill_box(target, name);
                }
            }
            KeyCode::Char('n') => {
                // Create a new session into this box (uses the full form).
                self.grid_overlay = GridOverlay::None;
                self.fill_target = Some(target);
                self.mode = Mode::Switcher;
                self.open_newform();
            }
            _ => {}
        }
    }

    // ── focus ────────────────────────────────────────────────────────────────
    /// Move focus to the nearest box in `dir` (spatial, by tile centers).
    fn focus_dir(&mut self, dir: Dir) {
        let rects = layout::tiles(self.layout, self.body_rect());
        if rects.len() < 2 {
            return;
        }
        let c = |r: &Rect| (r.x as i32 + r.width as i32 / 2, r.y as i32 + r.height as i32 / 2);
        let (cx, cy) = c(&rects[self.active]);
        let mut best: Option<usize> = None;
        let mut best_score = i32::MAX;
        for (i, r) in rects.iter().enumerate() {
            if i == self.active {
                continue;
            }
            let (x, y) = c(r);
            let aligned = match dir {
                Dir::Left => x < cx,
                Dir::Right => x > cx,
                Dir::Up => y < cy,
                Dir::Down => y > cy,
            };
            if !aligned {
                continue;
            }
            // Distance along the direction, with a penalty for off-axis boxes.
            let (primary, perp) = match dir {
                Dir::Left | Dir::Right => ((cx - x).abs(), (cy - y).abs()),
                Dir::Up | Dir::Down => ((cy - y).abs(), (cx - x).abs()),
            };
            let score = primary + perp * 4;
            if score < best_score {
                best_score = score;
                best = Some(i);
            }
        }
        if let Some(i) = best {
            self.active = i;
        }
    }

    fn body_rect(&self) -> Rect {
        Rect::new(0, 0, self.area.0, self.area.1.saturating_sub(1))
    }

    /// The box index whose tile contains a screen cell (None for the bar row).
    fn box_at(&self, col: u16, row: u16) -> Option<usize> {
        layout::tiles(self.layout, self.body_rect()).iter().position(|r| {
            col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    fn is_prefix(&self, k: KeyEvent) -> bool {
        let relevant = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        k.code == self.prefix.0 && (k.modifiers & relevant) == self.prefix.1
    }

    fn send_key_to_active(&mut self, k: KeyEvent) {
        if let Some(p) = self.panes.get_mut(self.active).and_then(|x| x.as_mut()) {
            if let Some(bytes) = input::encode(k, p.application_cursor()) {
                p.scroll_to_live(); // typing returns you to the live bottom
                p.send_input(bytes);
            }
        }
    }

    // ── attach / fill / layout ───────────────────────────────────────────────
    fn attach(&mut self) {
        let Some(s) = self.sessions.get(self.selected) else {
            return;
        };
        let session = s.name.clone();
        let target = self.fill_target.take().unwrap_or(0).min(self.panes.len().saturating_sub(1));
        self.fill_box(target, session);
    }

    fn fill_box(&mut self, idx: usize, session: String) {
        if idx >= self.panes.len() {
            return;
        }
        // Dedupe: a session may live in at most one box (else they fight over
        // the single PTY's width).
        for (j, slot) in self.panes.iter_mut().enumerate() {
            if j != idx && slot.as_ref().is_some_and(|p| p.session == session) {
                *slot = None;
            }
        }
        let (cols, rows) = self.box_size(idx);
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let (out_tx, out_rx) = mpsc::channel::<WsOut>(1024);
        let url = self.rest.urls().ws(&session);
        let task = tokio::spawn(ws::run(url, id, cols, rows, out_rx, self.tx.clone()));

        self.remember(&session);
        self.panes[idx] = Some(Pane::new(id, session, cols, rows, out_tx, task));
        self.active = idx;
        self.mode = Mode::Grid;
        self.prefix_armed = false;
    }

    fn set_layout(&mut self, l: Layout) {
        let n = l.count();
        // Web parity: migrate the focused box into slot 0 so what you're looking
        // at stays the primary box across the layout change.
        if self.active != 0 && self.active < self.panes.len() {
            self.panes.swap(0, self.active);
        }
        self.active = 0;
        self.layout = l;
        if self.panes.len() > n {
            self.panes.truncate(n); // dropped panes abort their WS via Drop
        } else {
            while self.panes.len() < n {
                self.panes.push(None);
            }
        }
        self.relayout();
    }

    fn detach_focused(&mut self) {
        if let Some(slot) = self.panes.get_mut(self.active) {
            *slot = None; // Drop aborts the WS task
        }
        self.after_box_removed();
    }

    fn kill_focused(&mut self) {
        if let Some(p) = self.panes.get(self.active).and_then(|x| x.as_ref()) {
            let rest = self.rest.clone();
            let target = p.session.clone();
            tokio::spawn(async move {
                let _ = rest.delete(&target, "kill").await;
            });
        }
        self.detach_focused();
    }

    /// After a box empties (detach / kill / session ended): if no boxes remain
    /// filled, fall back to the switcher in `Single`.
    fn after_box_removed(&mut self) {
        if self.panes.iter().all(|p| p.is_none()) {
            self.mode = Mode::Switcher;
            self.set_layout(Layout::Single);
            self.active = 0;
            self.pending_refresh = true;
        }
    }

    fn handle_pane(&mut self, id: u64, msg: PaneMsg) {
        for slot in self.panes.iter_mut() {
            if let Some(p) = slot {
                if p.id == id {
                    match msg {
                        PaneMsg::Bytes(b) => p.process(&b),
                        PaneMsg::State(s) => p.set_state(s),
                    }
                    return;
                }
            }
        }
    }

    /// Resize every box's emulator + PTY to its current tile (idempotent).
    fn relayout(&mut self) {
        let body = Rect::new(0, 0, self.area.0, self.area.1.saturating_sub(1));
        let inners = layout::inner_rects(self.layout, body);
        for (i, slot) in self.panes.iter_mut().enumerate() {
            if let (Some(p), Some(r)) = (slot.as_mut(), inners.get(i)) {
                p.resize(r.width, r.height);
            }
        }
    }

    fn box_size(&self, idx: usize) -> (u16, u16) {
        let body = Rect::new(0, 0, self.area.0, self.area.1.saturating_sub(1));
        layout::inner_rects(self.layout, body)
            .get(idx)
            .map(|r| (r.width.max(1), r.height.max(1)))
            .unwrap_or((80, 24))
    }

    /// Record a freshly-attached session as the most recent (best-effort save).
    fn remember(&mut self, session: &str) {
        self.cfg.recents.retain(|s| s != session);
        self.cfg.recents.insert(0, session.to_string());
        self.cfg.recents.truncate(20);
        let _ = self.cfg.save();
    }

    // ── render ───────────────────────────────────────────────────────────────
    fn render(&self, f: &mut Frame) {
        match self.mode {
            Mode::Switcher => {
                ui::switcher::render(f, self);
                match &self.overlay {
                    Overlay::None => {}
                    Overlay::Confirm { session, graceful } => {
                        let verb = if *graceful { "exit" } else { "kill" };
                        ui::overlay::confirm(f, " confirm ", &format!("{verb} session {session}?"));
                    }
                    Overlay::NewSession(form) => {
                        let tool =
                            self.tools.get(form.tool_idx).map(|t| t.prefix.as_str()).unwrap_or("-");
                        ui::overlay::new_session(
                            f,
                            &ui::overlay::NewSessionView {
                                tool,
                                name: &form.name,
                                dir: &form.dir,
                                field: form.field as usize,
                                error: form.error.as_deref(),
                            },
                        );
                    }
                }
            }
            Mode::Grid => {
                ui::grid::render(
                    f,
                    self.layout,
                    &self.panes,
                    self.active,
                    &self.prefix_label(),
                    self.prefix_armed,
                );
                match self.grid_overlay {
                    GridOverlay::None => {}
                    GridOverlay::Palette(hi) => ui::overlay::layout_palette(f, hi),
                    GridOverlay::Pick { target, selected } => ui::overlay::session_picker(
                        f,
                        &self.sessions,
                        selected,
                        target + 1,
                        self.panes.len(),
                    ),
                }
            }
        }
    }

    /// Human label for the prefix key, e.g. `^A` or `M-x`.
    fn prefix_label(&self) -> String {
        let key = match self.prefix.0 {
            KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
            _ => "?".into(),
        };
        if self.prefix.1.contains(KeyModifiers::CONTROL) {
            format!("^{key}")
        } else if self.prefix.1.contains(KeyModifiers::ALT) {
            format!("M-{key}")
        } else {
            key
        }
    }

    // ── UI accessors (switcher) ──────────────────────────────────────────────
    pub fn sessions(&self) -> &[SessionInfo] {
        &self.sessions
    }
    pub fn selected(&self) -> usize {
        self.selected
    }
    pub fn status(&self) -> &str {
        &self.status
    }
}

/// First line of an error chain — keeps the status bar to one line.
fn short_err(e: &anyhow::Error) -> String {
    e.to_string().lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
impl App {
    /// Build an app with a fixed session list + status for render tests (no
    /// network — `Rest` only builds an HTTP client, it doesn't connect).
    pub fn test_fixture(sessions: Vec<SessionInfo>, status: &str) -> Self {
        let rest = Rest::new("http://127.0.0.1:9", false).unwrap();
        let mut a = App::new(rest, Config::default());
        a.sessions = sessions;
        a.status = status.into();
        a
    }
}
