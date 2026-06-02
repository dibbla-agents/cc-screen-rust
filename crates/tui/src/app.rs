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
enum GridOverlay {
    None,
    Palette(usize), // highlighted index in Layout::ALL
    /// The unified action menu for box `target`; `selected` indexes
    /// `menu_items(sessions.len())`.
    Menu { target: usize, selected: usize },
    /// Inline new-session form that fills box `target` on submit.
    NewForm { target: usize, form: NewForm },
}

/// One selectable row of the grid action menu, in visual (top→bottom) order:
/// Change layout, New session, the sessions, Clear this box, Quit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuItem {
    ChangeLayout,
    NewSession,
    Session(usize),
    ClearBox,
    Quit,
}

/// The selectable menu rows for `session_count` sessions. Length is always
/// `session_count + 4`, so navigation wrapping is a plain modulo.
fn menu_items(session_count: usize) -> Vec<MenuItem> {
    let mut v = Vec::with_capacity(session_count + 4);
    v.push(MenuItem::ChangeLayout);
    v.push(MenuItem::NewSession);
    v.extend((0..session_count).map(MenuItem::Session));
    v.push(MenuItem::ClearBox);
    v.push(MenuItem::Quit);
    v
}

/// Initial menu cursor: the box's current session if it's in the list, else the
/// first session, else New session.
fn menu_initial(sessions: &[SessionInfo], current: Option<&str>) -> usize {
    current
        .and_then(|name| sessions.iter().position(|s| s.name == name))
        .map(|i| 2 + i)
        .or((!sessions.is_empty()).then_some(2))
        .unwrap_or(1)
}

/// Outcome of feeding a key to the shared new-session form.
enum NewFormAction {
    None,
    Submit,
    Cancel,
}

/// Apply one key to a `NewForm` — shared by the switcher form and the grid form.
fn newform_key(form: &mut NewForm, tools_len: usize, k: KeyEvent) -> NewFormAction {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, _) => return NewFormAction::Cancel,
        (KeyCode::Enter, _) => return NewFormAction::Submit,
        (KeyCode::Tab, _) | (KeyCode::BackTab, _) => form.field ^= 1,
        (KeyCode::Left, _) if tools_len > 0 => {
            form.tool_idx = (form.tool_idx + tools_len - 1) % tools_len;
        }
        (KeyCode::Right, _) if tools_len > 0 => {
            form.tool_idx = (form.tool_idx + 1) % tools_len;
        }
        (KeyCode::Backspace, _) => {
            if form.field == 0 {
                form.name.pop();
            } else {
                form.dir.pop();
            }
        }
        (KeyCode::Char(c), m)
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            if form.field == 0 {
                form.name.push(c);
            } else {
                form.dir.push(c);
            }
        }
        _ => {}
    }
    NewFormAction::None
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
        self.start_in_menu();
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
                self.grid_overlay = GridOverlay::None;
                // If the create was launched to fill a box, drop it in there;
                // otherwise it was a plain switcher create.
                if let Some(target) = self.fill_target.take() {
                    self.fill_box(target, name);
                } else {
                    self.status = format!("created {name}");
                }
                self.pending_refresh = true;
            }
            // Surface the error on whichever new-session form is open.
            Err(e) => {
                if let Overlay::NewSession(f) = &mut self.overlay {
                    f.error = Some(e);
                } else if let GridOverlay::NewForm { form, .. } = &mut self.grid_overlay {
                    form.error = Some(e);
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
                    // Click focuses the box; clicking an empty one opens the menu.
                    Down(_) => {
                        if let Some(idx) = self.box_at(me.column, me.row) {
                            self.active = idx;
                            if self.panes.get(idx).and_then(|x| x.as_ref()).is_none() {
                                self.open_menu(idx);
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
        let tools_len = self.tools.len();
        let Overlay::NewSession(form) = &mut self.overlay else {
            return;
        };
        match newform_key(form, tools_len, k) {
            NewFormAction::None => {}
            NewFormAction::Cancel => self.overlay = Overlay::None,
            NewFormAction::Submit => self.submit_newform(),
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

    /// Spawn the create request for `form`; the result arrives as
    /// `AppMsg::Created` and is routed by `handle_created`.
    fn spawn_create(&self, form: &NewForm) {
        let Some(t) = self.tools.get(form.tool_idx) else {
            return;
        };
        let req = CreateReq {
            tool: t.prefix.clone(),
            name: form.name.clone(),
            dir: form.dir.clone(),
            extra_dirs: Vec::new(),
        };
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = rest.create(&req).await.map_err(|e| e.to_string());
            let _ = tx.send(AppMsg::Created(r)).await;
        });
    }

    fn submit_newform(&mut self) {
        if let Overlay::NewSession(form) = &self.overlay {
            self.spawn_create(form);
        }
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
        // A grid overlay, when open, captures all keys. (Match a discriminant so
        // the borrow ends before we dispatch — GridOverlay isn't Copy.)
        let overlay = match &self.grid_overlay {
            GridOverlay::None => 0,
            GridOverlay::Palette(_) => 1,
            GridOverlay::Menu { .. } => 2,
            GridOverlay::NewForm { .. } => 3,
        };
        match overlay {
            1 => return self.key_palette(k),
            2 => return self.key_menu(k),
            3 => return self.key_grid_newform(k),
            _ => {}
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            if self.is_prefix(k) {
                self.send_key_to_active(k); // prefix prefix → literal prefix
                return;
            }
            match k.code {
                KeyCode::Char('d') => self.open_menu(self.active),
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
            self.open_menu(self.active); // empty box → the action menu
        }
    }

    // ── layout palette ───────────────────────────────────────────────────────
    fn open_palette(&mut self) {
        let cur = Layout::ALL.iter().position(|&l| l == self.layout).unwrap_or(0);
        self.grid_overlay = GridOverlay::Palette(cur);
        self.prefix_armed = false;
    }

    fn key_palette(&mut self, k: KeyEvent) {
        let hi = if let GridOverlay::Palette(hi) = &self.grid_overlay { *hi } else { return };
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

    /// Startup: go straight into the grid with the action menu open — attached
    /// to the first session if there is one, otherwise an empty box (the menu's
    /// New session / Quit still work, and clearing the empty box falls back to
    /// the switcher).
    fn start_in_menu(&mut self) {
        match self.sessions.first() {
            Some(first) => {
                let name = first.name.clone();
                self.fill_box(0, name); // → Grid mode, box 0
            }
            None => self.mode = Mode::Grid,
        }
        self.open_menu(0);
    }

    // ── unified action menu (Ctrl-A d / empty box) ───────────────────────────
    fn open_menu(&mut self, target: usize) {
        let target = target.min(self.panes.len().saturating_sub(1));
        let current = self.panes.get(target).and_then(|p| p.as_ref()).map(|p| p.session.clone());
        let selected = menu_initial(&self.sessions, current.as_deref());
        self.grid_overlay = GridOverlay::Menu { target, selected };
        self.prefix_armed = false;
    }

    fn key_menu(&mut self, k: KeyEvent) {
        let (target, selected) = match &self.grid_overlay {
            GridOverlay::Menu { target, selected } => (*target, *selected),
            _ => return,
        };
        let len = menu_items(self.sessions.len()).len(); // always sessions + 4
        match k.code {
            KeyCode::Esc => self.grid_overlay = GridOverlay::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.grid_overlay = GridOverlay::Menu { target, selected: (selected + len - 1) % len };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.grid_overlay = GridOverlay::Menu { target, selected: (selected + 1) % len };
            }
            KeyCode::Enter => {
                let item = menu_items(self.sessions.len()).get(selected).copied();
                if let Some(item) = item {
                    self.activate_menu(target, item);
                }
            }
            _ => {}
        }
    }

    fn activate_menu(&mut self, target: usize, item: MenuItem) {
        match item {
            // Hand off to the existing centered modals.
            MenuItem::ChangeLayout => self.open_palette(),
            MenuItem::NewSession => self.open_grid_newform(target),
            MenuItem::Session(i) => {
                if let Some(s) = self.sessions.get(i) {
                    let name = s.name.clone();
                    self.grid_overlay = GridOverlay::None;
                    self.fill_box(target, name);
                }
            }
            MenuItem::ClearBox => {
                self.grid_overlay = GridOverlay::None;
                self.clear_box(target);
            }
            MenuItem::Quit => self.should_quit = true,
        }
    }

    // ── inline new-session form (fills a box on submit) ───────────────────────
    fn open_grid_newform(&mut self, target: usize) {
        if self.tools.is_empty() {
            self.status = "no tools available".into();
            return;
        }
        self.grid_overlay = GridOverlay::NewForm {
            target,
            form: NewForm {
                tool_idx: 0,
                field: 0,
                name: String::new(),
                dir: self.home.clone(),
                error: None,
            },
        };
    }

    fn key_grid_newform(&mut self, k: KeyEvent) {
        let tools_len = self.tools.len();
        let (target, action) = match &mut self.grid_overlay {
            GridOverlay::NewForm { target, form } => (*target, newform_key(form, tools_len, k)),
            _ => return,
        };
        match action {
            NewFormAction::None => {}
            NewFormAction::Cancel => self.grid_overlay = GridOverlay::None,
            NewFormAction::Submit => {
                // Keep the form open until Created lands (handle_created routes
                // success into the box and failure back into the form).
                self.fill_target = Some(target);
                if let GridOverlay::NewForm { form, .. } = &self.grid_overlay {
                    self.spawn_create(form);
                }
            }
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

    /// Remove the session view from box `target` (the PTY keeps running on the
    /// server; Drop just aborts the local WS task).
    fn clear_box(&mut self, target: usize) {
        if let Some(slot) = self.panes.get_mut(target) {
            *slot = None;
        }
        self.after_box_removed();
    }

    fn detach_focused(&mut self) {
        self.clear_box(self.active);
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
                match &self.grid_overlay {
                    GridOverlay::None => {}
                    GridOverlay::Palette(hi) => ui::overlay::layout_palette(f, *hi),
                    GridOverlay::Menu { target, selected } => ui::overlay::grid_menu(
                        f,
                        &ui::overlay::MenuView {
                            sessions: &self.sessions,
                            selected: *selected,
                            box_num: *target + 1,
                            box_count: self.panes.len(),
                        },
                    ),
                    GridOverlay::NewForm { form, .. } => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn sess(name: &str) -> SessionInfo {
        SessionInfo {
            name: name.into(),
            tool: "claude".into(),
            short: name.into(),
            attached: false,
            activity: 0,
            preview: String::new(),
            cwd: String::new(),
        }
    }

    #[test]
    fn menu_items_order_and_length() {
        let it = menu_items(2);
        assert_eq!(it.len(), 6); // 2 sessions + 4 actions
        assert_eq!(
            it,
            vec![
                MenuItem::ChangeLayout,
                MenuItem::NewSession,
                MenuItem::Session(0),
                MenuItem::Session(1),
                MenuItem::ClearBox,
                MenuItem::Quit,
            ]
        );
        // No sessions still yields the four action rows.
        assert_eq!(menu_items(0).len(), 4);
    }

    #[test]
    fn menu_initial_prefers_current_then_first_then_new() {
        let list = vec![sess("a"), sess("b"), sess("c")];
        assert_eq!(menu_initial(&list, Some("b")), 3); // 2 + index 1
        assert_eq!(menu_initial(&list, Some("missing")), 2); // falls back to first session
        assert_eq!(menu_initial(&list, None), 2); // first session
        assert_eq!(menu_initial(&[], None), 1); // New session when there are none
    }

    #[test]
    fn menu_navigation_wraps_over_the_whole_list() {
        // 1 session → [layout, new, session0, clear, quit] = len 5.
        let len = menu_items(1).len();
        assert_eq!(len, 5);
        assert_eq!((0 + len - 1) % len, len - 1); // up from the top wraps to Quit
        assert_eq!((len - 1 + 1) % len, 0); // down from the bottom wraps to the top
    }

    fn form() -> NewForm {
        NewForm { tool_idx: 0, field: 0, name: String::new(), dir: String::new(), error: None }
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn newform_key_edits_cycles_submits_and_cancels() {
        let mut f = form();
        assert!(matches!(newform_key(&mut f, 3, key(KeyCode::Char('x'))), NewFormAction::None));
        assert_eq!(f.name, "x");
        newform_key(&mut f, 3, key(KeyCode::Tab)); // → dir field
        newform_key(&mut f, 3, key(KeyCode::Char('/')));
        assert_eq!(f.dir, "/");
        newform_key(&mut f, 3, key(KeyCode::Right)); // cycle tool
        assert_eq!(f.tool_idx, 1);
        newform_key(&mut f, 3, key(KeyCode::Left)); // and back
        assert_eq!(f.tool_idx, 0);
        assert!(matches!(newform_key(&mut f, 3, key(KeyCode::Enter)), NewFormAction::Submit));
        assert!(matches!(newform_key(&mut f, 3, key(KeyCode::Esc)), NewFormAction::Cancel));
    }
}
