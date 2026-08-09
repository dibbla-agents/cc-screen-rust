//! Top-level app state + a single unified event loop. All inputs — terminal
//! events, the 1 s poll tick, pane WebSocket bytes, and async action results —
//! funnel into one `mpsc<AppMsg>` channel. Two modes: the session switcher
//! (with modal overlays) and the tiled grid of attached boxes.

use std::collections::HashSet;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use cc_screen_protocol::{CreateReq, MachineInfo, RestorableSession, SessionInfo, ToolInfo};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::client::{ws, DirEntry, Rest};
use crate::config::Config;
use crate::input;
use crate::layout::{self, Layout};
use crate::pane::{ConnState, Pane, WsOut};
use crate::ready::{self, ReadyEdge};
use crate::ui;

/// How long a foreground ready-toast (0018 §3) stays up before the 1 s ticker
/// auto-dismisses it. A fresh edge for the same session resets the clock.
const TOAST_TTL: Duration = Duration::from_secs(8);

/// How long a transient statusbar hint (0069 Part D) stays up. Short — it's a
/// "that key did nothing, here's why" note, not a notification.
const HINT_TTL: Duration = Duration::from_secs(4);

/// Current Unix time in seconds (0 on the impossible pre-epoch clock), for the
/// ready-edge gates. Kept tiny so the detector itself stays pure + testable.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Why a `ccs <session>` direct-attach query (0059 C2) couldn't be resolved to a
/// single session. `Ambiguous` carries the human-readable candidate labels
/// (`machine/name`, or just `name` in single-agent mode) so the CLI can print
/// them to stderr before exiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    NotFound,
    Ambiguous(Vec<String>),
}

/// Case-insensitive subsequence test: do `query`'s chars appear, in order, in
/// `hay`? The last (fuzzy) tier of `resolve_attach`.
fn fuzzy_subseq(hay: &str, query: &str) -> bool {
    let mut q = query.chars().flat_map(char::to_lowercase).peekable();
    for h in hay.chars().flat_map(char::to_lowercase) {
        match q.peek() {
            Some(&qc) if qc == h => {
                q.next();
            }
            _ => {}
        }
        if q.peek().is_none() {
            return true;
        }
    }
    q.peek().is_none()
}

/// Display label for a session in ambiguity messages: `machine/name` on a hub,
/// bare `name` for a single unnamed agent.
fn attach_label(s: &SessionInfo) -> String {
    if s.machine.is_empty() {
        s.name.clone()
    } else {
        format!("{}/{}", s.machine, s.name)
    }
}

/// Resolve a `ccs <session>` positional query (0059 C2) to a single
/// `(name, machine)` target. Precedence, first tier with any match wins:
///   1. exact `machine/name`
///   2. exact `name`
///   3. unique `name` prefix
///   4. unique fuzzy (case-insensitive subsequence) `name` match
/// Exactly one hit in the winning tier ⇒ `Ok`; more than one ⇒ `Ambiguous` (the
/// candidates); no hit in any tier ⇒ `NotFound`. Pure — unit-tested below and
/// reused by both the CLI pre-flight (`main.rs`) and `App::start_in_menu`.
pub fn resolve_attach(sessions: &[SessionInfo], query: &str) -> Result<(String, String), AttachError> {
    let pick = |hits: &[&SessionInfo]| -> Option<Result<(String, String), AttachError>> {
        match hits {
            [] => None,
            [one] => Some(Ok((one.name.clone(), one.machine.clone()))),
            many => Some(Err(AttachError::Ambiguous(many.iter().map(|s| attach_label(s)).collect()))),
        }
    };

    // 1. exact machine/name (only when the query carries a slash).
    if let Some((machine, name)) = query.split_once('/') {
        let hits: Vec<&SessionInfo> =
            sessions.iter().filter(|s| s.machine == machine && s.name == name).collect();
        if let Some(r) = pick(&hits) {
            return r;
        }
    }
    // 2. exact name.
    let hits: Vec<&SessionInfo> = sessions.iter().filter(|s| s.name == query).collect();
    if let Some(r) = pick(&hits) {
        return r;
    }
    // 3. unique name prefix.
    let hits: Vec<&SessionInfo> = sessions.iter().filter(|s| s.name.starts_with(query)).collect();
    if let Some(r) = pick(&hits) {
        return r;
    }
    // 4. unique fuzzy (subsequence) name match.
    let hits: Vec<&SessionInfo> = sessions.iter().filter(|s| fuzzy_subseq(&s.name, query)).collect();
    pick(&hits).unwrap_or(Err(AttachError::NotFound))
}

/// The name to show for a session (proposal 0036 / 0059 C1): the operator-chosen
/// display label when set (and non-empty), else the slug `short`. Display-only —
/// it never replaces the identity `name`/`short`, so routing keys are untouched.
pub fn display_name(s: &SessionInfo) -> &str {
    match s.label.as_deref() {
        Some(l) if !l.is_empty() => l,
        _ => &s.short,
    }
}

/// Everything the event loop reacts to.
pub enum AppMsg {
    Term(Event),
    Tick,
    /// Bytes/state from a pane's WS task, tagged with the pane's id.
    Pane { id: u64, msg: PaneMsg },
    /// Result of an async create: Ok((session name, machine)) or Err(message).
    /// The machine rides along so a fill-a-box create attaches to the right agent.
    Created(Result<(String, String), String>),
    /// Subdirectories of `parent` for the new-session dir autocomplete.
    DirCands { parent: String, entries: Vec<DirEntry> },
    /// The tool list for the form's selected machine (re-fetched on a hub when
    /// the machine changes, since tools are per-agent).
    ToolsLoaded(Vec<ToolInfo>),
    /// Result of an async rename (0059 C1): Ok closes the overlay + refreshes;
    /// Err(message) keeps the rename overlay open with the error shown.
    Labeled(Result<(), String>),
    /// The restorable-session list for the restore picker (0059 C5). Empty (or a
    /// failed fetch) shows a status message instead of opening the overlay.
    Restorable(Vec<RestorableSession>),
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
    /// The unified action menu for box `target` — search-first per proposal
    /// 0062b (the grid analogue of the switcher). `selected` indexes the
    /// SELECTABLE rows of `menu_rows(_, &query)` (headers skipped); `query` is
    /// the menu's own type-to-search text (independent of the switcher's).
    Menu { target: usize, selected: usize, query: String },
    /// Inline new-session form that fills box `target` on submit.
    NewForm { target: usize, form: NewForm },
    /// Rename overlay for the focused box's session (0059 C1), reached from the
    /// action menu. Reuses the switcher rename form + text-input logic.
    Rename(RenameForm),
}

/// An action row of the grid menu, in resting visual order: Change layout and
/// New session above the sessions, Rename / Clear this box / Quit below.
/// (Sessions themselves are `MenuRow::Session` — 0062b split them out so the
/// filtered list can interleave actions and sessions purely by score.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuItem {
    ChangeLayout,
    NewSession,
    /// Rename the focused box's session (0059 C1). Present only when the box holds
    /// a session (`can_rename`), between the session list and Clear this box.
    RenameSession,
    ClearBox,
    Quit,
}

/// Alias terms that let the menu's action rows surface from a fuzzy query
/// (proposal 0062b, mirroring the web sidebar's `ACTION_TERMS` — "split" →
/// Change layout). An action's score is the raw best-over-aliases
/// `fuzzy_score_web` with NO tier base, so a NAME-tier session hit always
/// outranks an action, exactly like the web. Invariant (the web's "The label
/// is matched too"): an action's *rendered* label must be among its aliases —
/// the TUI labels the layout action "Change layout" where the web says "New
/// layout", so it carries both.
fn action_terms(item: MenuItem) -> &'static [&'static str] {
    match item {
        MenuItem::NewSession => &["new session", "create", "start"],
        MenuItem::ChangeLayout => {
            &["change layout", "new layout", "split", "grid", "tile", "panes"]
        }
        MenuItem::RenameSession => &["rename session", "label"],
        MenuItem::ClearBox => &["clear this box", "detach", "empty"],
        MenuItem::Quit => &["quit ccs", "exit"],
    }
}

/// One display row of the grid action menu (0062b): an action, a per-machine
/// group header (non-selectable, resting multi-machine hub mode only), or a
/// session referenced by its index into `App::sessions()`.
pub enum MenuRow {
    Action(MenuItem),
    Header { label: String, online: bool },
    Session(usize),
}

/// Number of selectable (non-header) rows — the menu cursor's modulus.
fn menu_selectable_len(rows: &[MenuRow]) -> usize {
    rows.iter().filter(|r| !matches!(r, MenuRow::Header { .. })).count()
}

/// The `nth` selectable row, skipping headers — how every menu action resolves
/// the cursor, so a header (or a filtered-out row) can never be acted on.
fn menu_selectable(rows: &[MenuRow], nth: usize) -> Option<&MenuRow> {
    rows.iter().filter(|r| !matches!(r, MenuRow::Header { .. })).nth(nth)
}

/// Map the menu cursor (a selectable index) to its row in the display list —
/// the menu's analogue of `ui::switcher::selected_row`.
fn menu_selected_row(rows: &[MenuRow], selected: usize) -> Option<usize> {
    let mut nth = 0usize;
    for (ri, row) in rows.iter().enumerate() {
        if !matches!(row, MenuRow::Header { .. }) {
            if nth == selected {
                return Some(ri);
            }
            nth += 1;
        }
    }
    None
}

/// Initial menu cursor: the box's current session if it's in the display order,
/// else the first session, else New session. `order` is the resting session
/// display order (`grouped_session_order`) — headers don't count, so session
/// number `i` in that order sits at selectable index `2 + i`.
fn menu_initial(sessions: &[SessionInfo], order: &[usize], current: Option<&str>) -> usize {
    current
        .and_then(|name| {
            order.iter().position(|&i| sessions.get(i).is_some_and(|s| s.name == name))
        })
        .map(|i| 2 + i)
        .or((!order.is_empty()).then_some(2))
        .unwrap_or(1)
}

/// Which field of the new-session form is focused. `Machine` is only in the cycle
/// when the server is a hub (`has_machine`); the two selector fields (`Tool`,
/// `Machine`) take ←/→, the two text fields (`Name`, `Dir`) take typed input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormField {
    Tool,
    Machine,
    Name,
    Dir,
    /// Per-session launch policy toggle (0005). Takes Space / ←→ to flip. (The
    /// hub-control toggle that lived beside it was retired by 0014.)
    SkipPermissions,
}

/// The fields in Tab order. `Machine` is skipped unless the server is a hub; the
/// skip-permissions toggle is always present.
fn form_fields(has_machine: bool) -> &'static [FormField] {
    use FormField::*;
    if has_machine {
        &[Tool, Machine, Name, Dir, SkipPermissions]
    } else {
        &[Tool, Name, Dir, SkipPermissions]
    }
}

/// Step `field` by `delta` (±1) through the active fields, wrapping.
fn step_field(field: FormField, has_machine: bool, delta: isize) -> FormField {
    let fields = form_fields(has_machine);
    let i = fields.iter().position(|&f| f == field).unwrap_or(0);
    let n = fields.len() as isize;
    fields[((i as isize + delta).rem_euclid(n)) as usize]
}

/// Outcome of feeding a key to the shared new-session form.
enum NewFormAction {
    None,
    Submit,
    Cancel,
    /// The dir field's parent changed — fetch its subdirectories (the result
    /// arrives as `AppMsg::DirCands` and refreshes the candidate list).
    FetchDirs(String),
    /// The selected machine changed — re-fetch both its dir listing (`parent`)
    /// and its tool list, since both are per-agent.
    MachineChanged(String),
}

/// Outcome of feeding a key to a rename overlay (0059 C1).
enum RenameAction {
    None,
    Submit,
    Cancel,
}

/// Apply one key to a `RenameForm` — shared by the switcher rename overlay and the
/// grid action-menu rename overlay. Same text-field logic as the new-session form:
/// printable→push, Backspace→pop, Esc→cancel, Enter→submit.
fn rename_key(form: &mut RenameForm, k: KeyEvent) -> RenameAction {
    match (k.code, k.modifiers) {
        (KeyCode::Esc, _) => RenameAction::Cancel,
        (KeyCode::Enter, _) => RenameAction::Submit,
        (KeyCode::Backspace, _) => {
            form.value.pop();
            RenameAction::None
        }
        (KeyCode::Char(c), m)
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            form.value.push(c);
            RenameAction::None
        }
        _ => RenameAction::None,
    }
}

/// Apply one key to a `NewForm` — shared by the switcher form and the grid form.
fn newform_key(form: &mut NewForm, tools_len: usize, machines_len: usize, k: KeyEvent) -> NewFormAction {
    let has_machine = machines_len > 0;
    match (k.code, k.modifiers) {
        (KeyCode::Esc, _) => return NewFormAction::Cancel,
        // In the dir field, an accepted candidate completes the path instead of
        // submitting / switching fields.
        (KeyCode::Enter, _) if form.field == FormField::Dir && form.dir_sel.is_some() => {
            return accept_candidate(form);
        }
        (KeyCode::Enter, _) => return NewFormAction::Submit,
        (KeyCode::Tab, _) if form.field == FormField::Dir && form.dir_sel.is_some() => {
            return accept_candidate(form);
        }
        (KeyCode::Tab, _) => form.field = step_field(form.field, has_machine, 1),
        (KeyCode::BackTab, _) => form.field = step_field(form.field, has_machine, -1),
        (KeyCode::Left, _) => match form.field {
            FormField::Tool if tools_len > 0 => {
                form.tool_idx = (form.tool_idx + tools_len - 1) % tools_len;
            }
            FormField::Machine if machines_len > 0 => {
                form.machine_idx = (form.machine_idx + machines_len - 1) % machines_len;
                return invalidate_dirs(form);
            }
            FormField::SkipPermissions => form.skip_permissions = !form.skip_permissions,
            _ => {}
        },
        (KeyCode::Right, _) => match form.field {
            FormField::Tool if tools_len > 0 => {
                form.tool_idx = (form.tool_idx + 1) % tools_len;
            }
            FormField::Machine if machines_len > 0 => {
                form.machine_idx = (form.machine_idx + 1) % machines_len;
                return invalidate_dirs(form);
            }
            FormField::SkipPermissions => form.skip_permissions = !form.skip_permissions,
            // → also accepts a highlighted dir candidate (a quick "drill in").
            FormField::Dir if form.dir_sel.is_some() => return accept_candidate(form),
            _ => {}
        },
        // Space toggles the focused policy switch (the text fields take it as a
        // literal char in the Char arm below).
        (KeyCode::Char(' '), _) if form.field == FormField::SkipPermissions => {
            form.skip_permissions = !form.skip_permissions;
        }
        (KeyCode::Down, _) if form.field == FormField::Dir => move_cand(form, 1),
        (KeyCode::Up, _) if form.field == FormField::Dir => move_cand(form, -1),
        (KeyCode::Backspace, _) => match form.field {
            FormField::Name => {
                form.name.pop();
            }
            FormField::Dir => {
                form.dir.pop();
                return after_dir_edit(form);
            }
            _ => {}
        },
        (KeyCode::Char(c), m)
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            match form.field {
                FormField::Name => form.name.push(c),
                FormField::Dir => {
                    form.dir.push(c);
                    return after_dir_edit(form);
                }
                _ => {}
            }
        }
        _ => {}
    }
    NewFormAction::None
}

/// Move the dir-candidate highlight (None → first → … and back to None at the
/// top), so the list both opens with ↓ and closes the highlight with ↑.
fn move_cand(form: &mut NewForm, delta: isize) {
    let n = form.dir_cands.len();
    if n == 0 {
        form.dir_sel = None;
        return;
    }
    form.dir_sel = match (form.dir_sel, delta) {
        (None, d) if d > 0 => Some(0),
        (None, _) => None,
        (Some(0), d) if d < 0 => None,
        (Some(i), d) => {
            let ni = (i as isize + d).clamp(0, n as isize - 1) as usize;
            Some(ni)
        }
    };
}

/// Accept the highlighted dir candidate: complete the path to that directory
/// (with a trailing slash so the next listing drills into it).
fn accept_candidate(form: &mut NewForm) -> NewFormAction {
    if let Some(c) = form.dir_sel.and_then(|i| form.dir_cands.get(i)) {
        form.dir = format!("{}/", c.path);
        form.dir_sel = None;
        return after_dir_edit(form);
    }
    NewFormAction::None
}

/// After switching machines: drop the cached dir listing (it belongs to the
/// previous agent) and signal that the tool list must be re-fetched too.
fn invalidate_dirs(form: &mut NewForm) -> NewFormAction {
    form.dir_parent.clear();
    form.dir_raw.clear();
    form.dir_cands.clear();
    form.dir_sel = None;
    NewFormAction::MachineChanged(dir_parent_of(&form.dir))
}

/// After the dir text changes: if the parent directory is unchanged, just
/// re-filter the cached listing; otherwise clear it and ask for the new parent.
fn after_dir_edit(form: &mut NewForm) -> NewFormAction {
    let parent = dir_parent_of(&form.dir);
    if parent == form.dir_parent {
        refilter_dirs(form);
        NewFormAction::None
    } else {
        form.dir_raw.clear();
        form.dir_cands.clear();
        form.dir_sel = None;
        NewFormAction::FetchDirs(parent)
    }
}

/// The directory to list for `dir`: everything up to the last `/`.
fn dir_parent_of(dir: &str) -> String {
    match dir.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => dir[..i].into(),
        None => String::new(),
    }
}

/// The fragment after the last `/` — what we fuzzy-match candidates against.
fn dir_partial_of(dir: &str) -> &str {
    match dir.rfind('/') {
        Some(i) => &dir[i + 1..],
        None => dir,
    }
}

/// fzf-ish-but-simpler match: higher is better, `None` = no match. Prefix beats
/// substring beats subsequence; shorter names win ties. An empty query matches.
fn fuzzy_score(name: &str, q: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(-(name.len() as i32));
    }
    let nl = name.to_lowercase();
    let ql = q.to_lowercase();
    if nl.starts_with(&ql) {
        return Some(1000 - name.len() as i32);
    }
    if let Some(pos) = nl.find(&ql) {
        return Some(500 - pos as i32 - name.len() as i32);
    }
    let mut chars = nl.chars();
    for qc in ql.chars() {
        if chars.position(|c| c == qc).is_none() {
            return None;
        }
    }
    Some(100 - name.len() as i32)
}

// ── switcher search (proposal 0062) ─────────────────────────────────────────

// Tiered field weighting for the switcher's type-to-search, ported from the web
// sidebar (SessionDrawer.tsx, proposal 0028): a session's score is
// `TIER_BASE + fuzzy_score_web(q, field)` for its best-matching field. The tier
// gap dwarfs any realistic fuzzy score (low hundreds), so a tier strictly
// dominates: a name hit always outranks a path-only hit, a path hit always
// outranks a summary/metadata-only hit; the fuzzy score only breaks ties
// *within* a tier. Keep the constants in lockstep with the frontend's.
const NAME_TIER: i64 = 100_000;
const PATH_TIER: i64 = 10_000;
const META_TIER: i64 = 0;

/// Port of the web sidebar's `fuzzyScore` (`frontend/src/util.ts`): greedy
/// leftmost case-insensitive subsequence match of `query` against `text`, with
/// the same bonuses — +2 per matched char, +6 for a head-of-string hit, +12 for
/// a word-start hit (after `/-_. `), +4·run for contiguous runs. `None` = not a
/// subsequence; an empty query scores 0 (matches everything). Kept separate
/// from `fuzzy_score` above (the dir-autocomplete scorer) because ranking
/// parity with the web is the contract here — both clients must order the same.
/// One deliberate divergence: this matches whole Unicode scalars, while the
/// JS side indexes the haystack per UTF-16 code unit, so an astral-plane query
/// char (emoji) can never match there — that's a latent web bug, not a spec.
fn fuzzy_score_web(query: &str, text: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = text.to_lowercase().chars().collect();
    let mut hi = 0usize;
    let mut score: i64 = 0;
    let mut last: isize = -2;
    let mut run: i64 = 0;
    for &qc in &q {
        let Some(pos) = (hi..h.len()).find(|&j| h[j] == qc) else {
            return None;
        };
        score += 2;
        if pos == 0 {
            score += 6;
        }
        if pos == 0 || "/-_. ".contains(h[pos - 1]) {
            score += 12;
        }
        if pos as isize == last + 1 {
            run += 1;
            score += 4 * run;
        } else {
            run = 0;
        }
        last = pos as isize;
        hi = pos + 1;
    }
    Some(score)
}

/// Best tiered score of `q` against one session (`None` = filtered out).
/// Fields and tiers mirror the web's `scoreItem`: NAME (`short`, `label`) >
/// PATH (cwd leaf, full cwd) > META (headline, detail, preview, tool, machine).
/// The cwd *leaf* is scored as its own PATH field so a folder literally named
/// the query outranks one that merely contains it as an ancestor.
/// `machine_label` is the resolved hostname (the header/chip text) so a query
/// matches a machine by either its id or its display name — a deliberate TUI
/// extra over the web (which scores only the raw id). It is ONLY searchable
/// for a real (non-empty) machine id: on a direct agent the id is `""` and the
/// label is the "this machine" *display* placeholder — scoring it would make
/// every session match queries like "mac" that the web drops.
fn score_session(s: &SessionInfo, machine_label: &str, q: &str) -> Option<i64> {
    let cwd_leaf = s.cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let machine_label = if s.machine.is_empty() { "" } else { machine_label };
    let fields: [(&str, i64); 10] = [
        (&s.short, NAME_TIER),
        (s.label.as_deref().unwrap_or(""), NAME_TIER),
        (cwd_leaf, PATH_TIER),
        (&s.cwd, PATH_TIER),
        (s.headline.as_deref().unwrap_or(""), META_TIER),
        (s.detail.as_deref().unwrap_or(""), META_TIER),
        (&s.preview, META_TIER),
        (&s.tool, META_TIER),
        (&s.machine, META_TIER),
        (machine_label, META_TIER),
    ];
    fields.iter().filter_map(|(f, base)| fuzzy_score_web(q, f).map(|sc| base + sc)).max()
}

/// One display row of the switcher list (proposal 0062 Part B): a per-machine
/// group header (hub mode, >1 machine, empty query) or a session, referenced
/// by its index into `App::sessions()`.
pub enum SwitcherRow {
    Header { label: String, online: bool },
    Session(usize),
}

/// Recompute the shown candidates from the cached listing + the current partial.
/// Hidden directories are excluded unless the partial itself starts with `.`.
fn refilter_dirs(form: &mut NewForm) {
    let partial = dir_partial_of(&form.dir);
    let show_hidden = partial.starts_with('.');
    let mut scored: Vec<(i32, DirEntry)> = form
        .dir_raw
        .iter()
        .filter(|e| show_hidden || !e.name.starts_with('.'))
        .filter_map(|e| fuzzy_score(&e.name, partial).map(|s| (s, e.clone())))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    form.dir_cands = scored.into_iter().map(|(_, e)| e).collect();
    form.dir_sel = None;
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
    /// Rename the selected session (0059 C1) — a single free-text field seeded
    /// with the current display name.
    RenameSession(RenameForm),
    /// A checklist of the sessions a redeploy left restorable (0059 C5).
    RestorePicker(RestoreForm),
}

/// The rename overlay's state (0059 C1). `session`/`machine` identify the target
/// (routing keys, never shown as editable); `value` is the edited display label.
struct RenameForm {
    session: String,
    machine: String,
    value: String,
    error: Option<String>,
}

/// The restorable-session picker's state (0059 C5). `selected` is parallel to
/// `items` (a per-row checkbox); `cursor` is the highlighted row. NOTE: restore is
/// all-or-nothing server-side, so the toggles are a preview/confirmation only —
/// see `submit_restore_picker`.
struct RestoreForm {
    items: Vec<RestorableSession>,
    selected: Vec<bool>,
    cursor: usize,
}

struct NewForm {
    tool_idx: usize,
    machine_idx: usize,
    field: FormField,
    name: String,
    dir: String,
    error: Option<String>,
    /// Dir autocomplete state. `dir_parent` is the path `dir_raw` was listed for
    /// (the cache key); `dir_cands` is `dir_raw` filtered by the current fragment;
    /// `dir_sel` is the highlighted candidate (None = none, so Enter submits).
    dir_parent: String,
    dir_raw: Vec<DirEntry>,
    dir_cands: Vec<DirEntry>,
    dir_sel: Option<usize>,
    /// Per-session launch policy (0005). Defaults to the agent's serde default:
    /// YOLO on. (The hub-control toggle beside it was retired by 0014.)
    skip_permissions: bool,
}

impl NewForm {
    /// A fresh form seeded with `dir` (trailing slash → its contents list first)
    /// and the default machine. The caller kicks off the initial dir fetch.
    fn new(dir: String, machine_idx: usize) -> Self {
        Self {
            tool_idx: 0,
            machine_idx,
            field: FormField::Name,
            name: String::new(),
            dir,
            error: None,
            dir_parent: String::new(),
            dir_raw: Vec::new(),
            dir_cands: Vec::new(),
            dir_sel: None,
            skip_permissions: true,
        }
    }
}

const MOUSE_STEP: isize = 3;
/// `g` (top) in keyboard-scroll mode: a delta far larger than any history
/// (alacritty caps scrollback at 10 000 lines) so the view lands on the oldest
/// line. `Pane::scroll` clamps to `[0, history]`.
const SCROLL_TOP: isize = 1_000_000;

pub struct App {
    rest: Rest,
    cfg: Config,
    tools: Vec<ToolInfo>,
    /// Connected agents (hub mode). Empty + `hub_mode == false` means a direct,
    /// single, unnamed agent — then the new-session form hides the machine row.
    machines: Vec<MachineInfo>,
    hub_mode: bool,
    /// In direct mode, this agent's own machine name (from `/api/session/root`),
    /// shown read-only in the new-session form. Empty when unknown / in hub mode.
    self_machine: String,
    home: String,
    sessions: Vec<SessionInfo>,
    /// The switcher's cursor as an index into `visible_sessions()` (the display
    /// order — ranked while filtering, grouped at rest). NOT a `sessions` index;
    /// every session-consuming action resolves through `selected_session()`.
    selected: usize,
    /// The switcher's type-to-search query (proposal 0062 Part A). Empty =
    /// the resting (grouped) list; non-empty = filtered + ranked.
    query: String,
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
    /// Keyboard-scroll mode on the focused pane (0059 C3), entered with `^A [`.
    /// While set, `key_grid` routes navigation keys to `Pane::scroll[_to_live]`
    /// instead of the input encoder; `q`/`Esc`/`G` clear it back to live.
    scroll_mode: bool,
    tx: mpsc::Sender<AppMsg>,
    rx: Option<mpsc::Receiver<AppMsg>>,
    should_quit: bool,
    pending_refresh: bool,
    /// A `ccs <session>` direct-attach query (0059 C2), if given. Resolved against
    /// the freshly-refreshed session list in `start_in_menu`: on a hit the app
    /// boots straight into the grid attached to that session (no action menu); a
    /// resolve miss falls back to the default menu boot.
    start_attach_query: Option<String>,

    // ── ready-session notifications (0018) ──────────────────────────────────
    /// Sessions that crossed the gated busy→waiting edge and are still ready,
    /// awaiting a `^A g` / click. Replace-not-stack by `(machine, name)`; empty
    /// = no toast. Rendered as a transient statusbar segment in the grid (§3).
    toast: Vec<ReadyEdge>,
    /// When the toast auto-dismisses (checked each 1 s `refresh`); `None` = no
    /// toast pending. No separate timer — it rides the existing ticker (§3).
    toast_until: Option<Instant>,
    /// Terminal focus (DECSET 1004), the TUI analog of `document.visibilityState`
    /// (§5). Drives the foreground/background split; defaults true so terminals
    /// without focus reporting always take the (harmless) statusbar toast.
    is_focused: bool,

    /// A transient grid statusbar note ("that key can't do anything here, and
    /// why") with its expiry — 0069 Part D's refused-`^A [` hint. Rides the
    /// existing 1 s ticker; the ready-toast outranks it when both are up.
    hint: Option<(String, Instant)>,
}

/// Which real-terminal side effects `App::run_with` starts. Production passes the
/// default (both on); e2e tests pass both off and drive the loop synthetically
/// through `App::tx()` (proposal 0059 B2).
#[derive(Clone, Copy, Debug)]
pub struct RunOpts {
    /// Spawn the crossterm `EventStream` reader task (real keyboard/mouse/resize).
    pub spawn_term_events: bool,
    /// Spawn the 1 s ticker that drives `AppMsg::Tick` (poll refresh, toast expiry).
    pub spawn_ticker: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self { spawn_term_events: true, spawn_ticker: true }
    }
}

impl App {
    pub fn new(rest: Rest, cfg: Config) -> Self {
        let (tx, rx) = mpsc::channel(512);
        let prefix = input::parse_prefix(&cfg.prefix);
        Self {
            rest,
            cfg,
            tools: Vec::new(),
            machines: Vec::new(),
            hub_mode: false,
            self_machine: String::new(),
            home: String::new(),
            sessions: Vec::new(),
            selected: 0,
            query: String::new(),
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
            scroll_mode: false,
            tx,
            rx: Some(rx),
            should_quit: false,
            pending_refresh: false,
            start_attach_query: None,
            toast: Vec::new(),
            toast_until: None,
            is_focused: true,
            hint: None,
        }
    }

    /// Clone the app's message sender. Tests drive the loop by pushing synthetic
    /// `AppMsg`s (keys, ticks, resizes) onto this channel instead of a real
    /// crossterm `EventStream` / ticker (see `RunOpts`).
    pub fn tx(&self) -> mpsc::Sender<AppMsg> {
        self.tx.clone()
    }

    /// Arm `ccs <session>` direct-attach (0059 C2): `query` is resolved against the
    /// session list at boot (`start_in_menu`), attaching box 0 straight into the
    /// grid instead of opening the action menu. `main.rs` sets the raw query it
    /// already pre-flighted for exit codes; `App` re-resolves via the same pure
    /// `resolve_attach`, so a miss (e.g. the session vanished) degrades to the
    /// default menu boot rather than erroring.
    pub fn set_start_attach(&mut self, query: String) {
        self.start_attach_query = Some(query);
    }

    pub async fn run<B: Backend>(self, term: &mut Terminal<B>) -> Result<()> {
        self.run_with(term, RunOpts::default()).await
    }

    /// The event loop, generic over the ratatui backend so an e2e test can run it
    /// against a `TestBackend` (proposal 0059 B2). `opts` gates the real-terminal
    /// side effects (the crossterm event stream + the 1 s ticker) so a test owns
    /// the stimulus and feeds the loop deterministically via `tx()`.
    pub async fn run_with<B: Backend>(mut self, term: &mut Terminal<B>, opts: RunOpts) -> Result<()> {
        let mut rx = self.take_rx();
        if opts.spawn_term_events {
            self.spawn_term_events();
        }
        if opts.spawn_ticker {
            self.spawn_ticker();
        }

        self.init().await;
        self.draw(term)?;

        while let Some(msg) = rx.recv().await {
            self.handle_msg(msg).await;
            while let Ok(m) = rx.try_recv() {
                self.handle_msg(m).await;
            }
            if self.should_quit {
                break;
            }
            self.draw(term)?;
        }
        Ok(())
    }

    // ── driver pieces, shared by `run_with` and the e2e harness (0059 B3) ────────
    //
    // A bin-only loop can't be single-stepped from a test. Splitting it into
    // `take_rx` / `init` / `handle_msg` / `draw` lets `tests/e2e.rs` push a
    // synthetic `AppMsg`, drain the channel, redraw a `TestBackend`, and assert on
    // the buffer — while production still runs the exact same pieces in `run_with`.

    /// Take the app's receiver (once). Production's `run_with` owns it; a test owns
    /// it so it can drain pane/create/tool messages between synthetic stimuli.
    pub fn take_rx(&mut self) -> mpsc::Receiver<AppMsg> {
        self.rx.take().expect("take_rx()/run() called once")
    }

    /// One-time async startup: resolve the server root, probe for a hub, load the
    /// default machine's tools, do the first session refresh, and pick the initial
    /// screen. No terminal side effects (no `spawn_*`) — those are `run_with`'s.
    pub async fn init(&mut self) {
        if let Ok((home, machine)) = self.rest.root_info().await {
            self.home = home;
            self.self_machine = machine;
        }
        // Probe for a hub: Some(list) → hub (show the machine picker), None → a
        // direct agent with no /api/machines route (single machine, named by
        // `self_machine`).
        if let Ok(Some(list)) = self.rest.machines().await {
            self.hub_mode = true;
            self.machines = list;
        }
        // Tools are per-agent; on a hub with >1 online machine a machine-less
        // request is ambiguous (returns `[]`, which disables New Session), so
        // fetch them for the machine the form will default to.
        self.tools = self.rest.tools(&self.default_machine_name()).await.unwrap_or_default();

        self.refresh().await;
        self.start_in_menu();
    }

    /// Handle one message (a key/tick/resize, or a pane/create/tools async result).
    pub async fn handle_msg(&mut self, msg: AppMsg) {
        self.handle(msg).await;
    }

    /// Re-pin the layout to the current terminal size and repaint one frame.
    pub fn draw<B: Backend>(&mut self, term: &mut Terminal<B>) -> Result<()> {
        self.sync_area(term);
        self.relayout();
        term.draw(|f| self.render(f))?;
        Ok(())
    }

    /// Whether the loop has been asked to quit (a test checks this after `q`).
    pub fn should_quit(&self) -> bool {
        self.should_quit
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

    fn sync_area<B: Backend>(&mut self, term: &Terminal<B>) {
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
            AppMsg::DirCands { parent, entries } => self.handle_dir_cands(parent, entries),
            AppMsg::ToolsLoaded(tools) => self.handle_tools_loaded(tools),
            AppMsg::Labeled(res) => self.handle_labeled(res),
            AppMsg::Restorable(list) => self.handle_restorable(list),
        }
        if self.pending_refresh {
            self.pending_refresh = false;
            self.refresh().await;
        }
    }

    // ── session list ─────────────────────────────────────────────────────────
    async fn refresh(&mut self) {
        // Keep the machine list fresh so agents going on/offline reflect in the
        // picker. Only when we know it's a hub (else there's no such route).
        if self.hub_mode {
            if let Ok(Some(list)) = self.rest.machines().await {
                self.machines = list;
            }
        }
        match self.rest.sessions().await {
            Ok(mut list) => {
                list.sort_by(|a, b| a.name.cmp(&b.name));
                // 0018: detect sessions that crossed the gated busy→waiting edge
                // between the previous and current snapshot, excluding any
                // mounted in a box (they carry their own status). The detector is
                // pure; the focus split + toast/bell plumbing is `note_ready_edges`.
                let mounted: HashSet<(String, String)> = self
                    .panes
                    .iter()
                    .filter_map(|p| p.as_ref())
                    .map(|p| (p.machine.clone(), p.session.clone()))
                    .collect();
                let edges = ready::detect_ready_edges(&self.sessions, &list, &mounted, now_secs());
                self.sessions = list;
                self.refresh_pane_accents();
                self.note_ready_edges(edges, &mounted);
                let visible = self.visible_sessions().len();
                if self.selected >= visible {
                    self.selected = visible.saturating_sub(1);
                }
                // Auto-detach any box whose session ended. Keyed by (machine,
                // name) so a session on one machine doesn't keep a box alive for
                // a same-named session on another.
                let live: HashSet<(&str, &str)> =
                    self.sessions.iter().map(|s| (s.machine.as_str(), s.name.as_str())).collect();
                let mut changed = false;
                for slot in self.panes.iter_mut() {
                    if slot
                        .as_ref()
                        .is_some_and(|p| !live.contains(&(p.machine.as_str(), p.session.as_str())))
                    {
                        *slot = None;
                        changed = true;
                    }
                }
                if changed {
                    self.after_box_removed();
                }
                // 0062b: an open action menu's cursor rides `menu_rows()`, which
                // can shrink under it on this refresh (sessions ended mid-filter)
                // — unclamped, the next frame has no highlighted row and Enter
                // is dead until a navigation key wraps it back. Clamp it like
                // the switcher's `selected` above. (Runs after the auto-detach
                // so a menu `after_box_removed` just cleared stays cleared.)
                if let GridOverlay::Menu { target, selected, query } = &self.grid_overlay {
                    let (target, selected, query) = (*target, *selected, query.clone());
                    let len =
                        menu_selectable_len(&self.menu_rows(self.box_has_session(target), &query));
                    if selected >= len {
                        self.grid_overlay = GridOverlay::Menu {
                            target,
                            selected: len.saturating_sub(1),
                            query,
                        };
                    }
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

    // ── ready-session notifications (0018) ───────────────────────────────────
    /// Act on the ready edges from a `refresh`, applying the focus split (§5):
    /// focused → a foreground statusbar toast (§3); unfocused → a terminal bell
    /// + OSC 9 desktop notification (§4). Also prunes/expires the standing toast
    /// so it never points at a gone, resumed, or now-mounted session.
    fn note_ready_edges(&mut self, edges: Vec<ReadyEdge>, mounted: &HashSet<(String, String)>) {
        // Auto-dismiss rides the 1 s ticker — no separate timer.
        if self.toast_until.is_some_and(|until| Instant::now() >= until) {
            self.toast.clear();
            self.toast_until = None;
        }
        // Drop toast entries that no longer warrant a jump: session ended,
        // resumed work (no longer waiting), or got mounted in a box.
        let still_ready: HashSet<(String, String)> = self
            .sessions
            .iter()
            .filter(|s| s.waiting)
            .map(|s| (s.machine.clone(), s.name.clone()))
            .collect();
        self.toast.retain(|e| still_ready.contains(&e.key()) && !mounted.contains(&e.key()));
        if self.toast.is_empty() {
            self.toast_until = None;
        }

        if edges.is_empty() {
            return;
        }
        if self.is_focused {
            // Foreground: a non-modal statusbar toast. Replace-not-stack by key,
            // mirroring the web's per-session replace.
            if self.cfg.notify.wants_toast() {
                for e in edges {
                    self.toast.retain(|x| x.key() != e.key());
                    self.toast.push(e);
                }
                self.toast_until = Some(Instant::now() + TOAST_TTL);
            }
        } else if self.cfg.notify.wants_bell() {
            // Background: a statusbar line helps no one — emit the out-of-app
            // signal instead. The standing §6 indicator surfaces them on refocus.
            self.emit_bell_osc(&edges);
        }
    }

    /// Emit a terminal BEL + an OSC 9 desktop notification for `edges` — the
    /// background analog of Web Push (§4). Written straight to stdout as
    /// out-of-band control bytes the terminal consumes; ratatui repaints the
    /// screen on the next draw. Not deep-linkable (terminals can't route a
    /// notification click back into a session) — the actionable jump is the §3
    /// toast the user sees once the terminal is focused again.
    fn emit_bell_osc(&self, edges: &[ReadyEdge]) {
        let label = match edges {
            [one] => format!("{} ready", one.short),
            many => format!("{} sessions ready", many.len()),
        };
        let mut out = std::io::stdout();
        // BEL raises the terminal's urgency hint; OSC 9 is the desktop toast on
        // iTerm2 / kitty / WezTerm and friends.
        let _ = write!(out, "\x07\x1b]9;cc-screen: {label}\x07");
        let _ = out.flush();
    }

    /// `^A g` / a toast click: jump to the ready session(s) (§3). One ready →
    /// mount it directly in the active box (the common case); several → open the
    /// switcher with the cursor on the first ready session (no ready-only filter
    /// — the §6 dots mark them). No-op when nothing is ready.
    fn jump_ready(&mut self) {
        if self.toast.is_empty() {
            return;
        }
        if self.toast.len() == 1 {
            let e = self.toast[0].clone();
            self.clear_toast();
            self.fill_box(self.active, e.name, e.machine);
        } else {
            let keys: HashSet<(String, String)> = self.toast.iter().map(|e| e.key()).collect();
            // A fresh switcher entry starts unfiltered; the cursor indexes the
            // visible (grouped) order, so resolve the ready session through it.
            self.clear_query();
            if let Some(pos) = self.visible_sessions().iter().position(|&i| {
                let s = &self.sessions[i];
                keys.contains(&(s.machine.clone(), s.name.clone()))
            }) {
                self.selected = pos;
            }
            self.clear_toast();
            self.grid_overlay = GridOverlay::None;
            self.mode = Mode::Switcher;
        }
    }

    fn clear_toast(&mut self) {
        self.toast.clear();
        self.toast_until = None;
    }

    /// The toast's rendered text (right-aligned in the grid statusbar), or `None`
    /// when no session is ready. Coalesced when several fire.
    fn toast_text(&self) -> Option<String> {
        if self.toast.is_empty() {
            return None;
        }
        let p = self.prefix_label();
        Some(if self.toast.len() == 1 {
            format!("✦ {} ready  {p} g to jump ", self.toast[0].short)
        } else {
            format!("✦ {} sessions ready  {p} g ", self.toast.len())
        })
    }

    /// The screen rect the toast occupies — the rightmost columns of the bottom
    /// statusbar row — for mouse hit-testing. `None` when no toast is up.
    fn toast_rect(&self) -> Option<Rect> {
        let t = self.toast_text()?;
        let w = (t.chars().count() as u16).min(self.area.0);
        Some(Rect::new(self.area.0.saturating_sub(w), self.area.1.saturating_sub(1), w, 1))
    }

    fn toast_hit(&self, col: u16, row: u16) -> bool {
        self.toast_rect().is_some_and(|r| {
            col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    fn handle_created(&mut self, res: Result<(String, String), String>) {
        match res {
            Ok((name, machine)) => {
                // Only dismiss the new-session overlay this reply belongs to — a slow
                // create must not wipe a rename/confirm overlay opened meanwhile.
                if matches!(self.overlay, Overlay::NewSession(_)) {
                    self.overlay = Overlay::None;
                }
                if matches!(self.grid_overlay, GridOverlay::NewForm { .. }) {
                    self.grid_overlay = GridOverlay::None;
                }
                // If the create was launched to fill a box, drop it in there on
                // the machine we routed it to; otherwise it was a plain switcher
                // create.
                if let Some(target) = self.fill_target.take() {
                    self.fill_box(target, name, machine);
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

    /// Adopt a freshly fetched tool list (for the form's current machine) and
    /// clamp the open form's tool selection so it can't point past the new list.
    fn handle_tools_loaded(&mut self, tools: Vec<ToolInfo>) {
        let max = tools.len().saturating_sub(1);
        self.tools = tools;
        match &mut self.overlay {
            Overlay::NewSession(f) => f.tool_idx = f.tool_idx.min(max),
            _ => {
                if let GridOverlay::NewForm { form, .. } = &mut self.grid_overlay {
                    form.tool_idx = form.tool_idx.min(max);
                }
            }
        }
    }

    /// Apply a dir listing to whichever new-session form is open, but only if its
    /// dir still wants that parent (a later keystroke may have moved on).
    fn handle_dir_cands(&mut self, parent: String, entries: Vec<DirEntry>) {
        let form = match &mut self.overlay {
            Overlay::NewSession(f) => Some(f),
            _ => match &mut self.grid_overlay {
                GridOverlay::NewForm { form, .. } => Some(form),
                _ => None,
            },
        };
        if let Some(f) = form {
            if dir_parent_of(&f.dir) == parent {
                f.dir_parent = parent;
                f.dir_raw = entries;
                refilter_dirs(f);
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
            // Terminal focus (DECSET 1004) drives the 0018 notification split
            // (§5): toast while focused, bell + OSC 9 while not.
            Event::FocusGained => self.is_focused = true,
            Event::FocusLost => self.is_focused = false,
            _ => {}
        }
    }

    /// The pane's content rect (inside the box border), for pane-local mouse
    /// coordinates. Falls back to the whole body if the index is out of range.
    fn box_rect(&self, idx: usize) -> Rect {
        let body = self.body_rect();
        layout::inner_rects(self.layout, body).get(idx).copied().unwrap_or(body)
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
                        let rect = self.box_rect(idx);
                        let up = matches!(me.kind, ScrollUp);
                        if let Some(p) = self.panes.get_mut(idx).and_then(|x| x.as_mut()) {
                            wheel(p, up, rect, me.column, me.row, me.modifiers);
                        }
                    }
                    // Click focuses the box; clicking an empty one opens the menu.
                    Down(_) => {
                        // A click on the statusbar toast jumps to the ready
                        // session(s) — the mouse path for ^A g (§3).
                        if self.toast_hit(me.column, me.row) {
                            self.jump_ready();
                            return;
                        }
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
            Overlay::RenameSession(_) => 3,
            Overlay::RestorePicker(_) => 4,
        };
        match kind {
            1 => self.key_confirm(k),
            2 => self.key_newform(k),
            3 => self.key_rename(k),
            4 => self.key_restore_picker(k),
            _ => self.key_list(k),
        }
    }

    // The search-first switcher (proposal 0062, amending 0059's key summary):
    // bare printables type into the query, so every letter command lives on a
    // Ctrl-chord. `q`/`j`/`k`/`r` are gone (Esc/Ctrl+C quit, arrows + wheel
    // navigate, the 1 s ticker already refreshes).
    fn key_list(&mut self, k: KeyEvent) {
        match (k.code, k.modifiers) {
            // Esc: clear an active query first; on an empty query cancel back to
            // the grid (fill-a-box) or quit — the web sidebar's clear-then-close
            // two-step.
            (KeyCode::Esc, _) => {
                if !self.query.is_empty() {
                    self.clear_query();
                } else if self.fill_target.take().is_some() {
                    self.mode = Mode::Grid;
                } else {
                    self.should_quit = true;
                }
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.clear_query(),
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => self.open_newform(),
            (KeyCode::Char('x'), KeyModifiers::CONTROL) => self.confirm_delete(false),
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => self.confirm_delete(true),
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => self.open_rename(),
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => self.open_restore_picker(),
            (KeyCode::Down, _) => self.move_sel(1),
            (KeyCode::Up, _) => self.move_sel(-1),
            (KeyCode::Enter, _) => self.attach(),
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.selected = 0;
            }
            // Type-to-search: any printable key (bare or shifted) appends to the
            // query and re-ranks immediately, cursor snapping to the top match.
            (KeyCode::Char(ch), m)
                if !m.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.query.push(ch);
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.selected = 0;
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
        let machines_len = self.machines.len();
        let Overlay::NewSession(form) = &mut self.overlay else {
            return;
        };
        match newform_key(form, tools_len, machines_len, k) {
            NewFormAction::None => {}
            NewFormAction::Cancel => self.overlay = Overlay::None,
            NewFormAction::Submit => self.submit_newform(),
            NewFormAction::FetchDirs(parent) => self.spawn_dir_fetch(parent),
            NewFormAction::MachineChanged(parent) => {
                self.spawn_dir_fetch(parent);
                self.spawn_tools_fetch();
            }
        }
    }

    fn move_sel(&mut self, delta: isize) {
        // Wraps over the *visible* (possibly filtered) list; group headers are
        // not part of it, so the cursor can never land on one.
        let n = self.visible_sessions().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
    }

    // ── lifecycle (create / kill / restore) ──────────────────────────────────
    /// The machine selected by default: the first online one, else the first.
    fn default_machine_idx(&self) -> usize {
        self.machines.iter().position(|m| m.online).unwrap_or(0)
    }

    /// The name of the default machine ("" in direct-agent mode / no machines).
    fn default_machine_name(&self) -> String {
        self.machines.get(self.default_machine_idx()).map(|m| m.machine.clone()).unwrap_or_default()
    }

    /// Re-fetch the tool list for whichever machine the open form now points at
    /// (tools are per-agent), posting the result back as `AppMsg::ToolsLoaded`.
    fn spawn_tools_fetch(&self) {
        let machine = self.form_machine();
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Ok(tools) = rest.tools(&machine).await {
                let _ = tx.send(AppMsg::ToolsLoaded(tools)).await;
            }
        });
    }

    /// The dir to seed a new form with: $HOME plus a trailing slash, so the very
    /// first listing shows home's contents to pick from.
    fn seed_dir(&self) -> String {
        if self.home.is_empty() {
            String::new()
        } else {
            format!("{}/", self.home.trim_end_matches('/'))
        }
    }

    fn open_newform(&mut self) {
        if self.tools.is_empty() {
            self.status = "no tools available".into();
            return;
        }
        let form = NewForm::new(self.seed_dir(), self.default_machine_idx());
        let parent = dir_parent_of(&form.dir);
        self.overlay = Overlay::NewSession(form);
        self.spawn_dir_fetch(parent);
    }

    /// Fetch the subdirectories of `parent` (on the form's selected machine) and
    /// post them back as `AppMsg::DirCands`.
    fn spawn_dir_fetch(&self, parent: String) {
        let machine = self.form_machine();
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let entries = rest.dirs(&parent, &machine).await.unwrap_or_default();
            let _ = tx.send(AppMsg::DirCands { parent, entries }).await;
        });
    }

    /// The machine name selected on whichever new-session form is open ("" when
    /// none open or in direct-agent mode).
    fn form_machine(&self) -> String {
        let idx = match &self.overlay {
            Overlay::NewSession(f) => Some(f.machine_idx),
            _ => match &self.grid_overlay {
                GridOverlay::NewForm { form, .. } => Some(form.machine_idx),
                _ => None,
            },
        };
        idx.and_then(|i| self.machines.get(i)).map(|m| m.machine.clone()).unwrap_or_default()
    }

    /// Spawn the create request for `form`; the result arrives as
    /// `AppMsg::Created` and is routed by `handle_created`.
    fn spawn_create(&self, form: &NewForm) {
        let Some(t) = self.tools.get(form.tool_idx) else {
            return;
        };
        let machine =
            self.machines.get(form.machine_idx).map(|m| m.machine.clone()).unwrap_or_default();
        let req = CreateReq {
            tool: t.prefix.clone(),
            name: form.name.clone(),
            dir: form.dir.clone(),
            extra_dirs: Vec::new(),
            skip_permissions: form.skip_permissions,
        };
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = rest
                .create(&req, &machine)
                .await
                .map(|name| (name, machine))
                .map_err(|e| e.to_string());
            let _ = tx.send(AppMsg::Created(r)).await;
        });
    }

    fn submit_newform(&mut self) {
        if let Overlay::NewSession(form) = &self.overlay {
            self.spawn_create(form);
        }
    }

    fn confirm_delete(&mut self, graceful: bool) {
        if let Some(session) = self.selected_session().map(|s| s.name.clone()) {
            self.overlay = Overlay::Confirm { session, graceful };
        }
    }

    fn restore_all(&mut self) {
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        // Route to the same agent the restorable list was fetched from (0059 C5) —
        // a machine-less restore 404s on a multi-agent hub. Mirrors `restorable`.
        let machine = self.default_machine_name();
        tokio::spawn(async move {
            let _ = rest.restore(&machine).await;
            let _ = tx.send(AppMsg::Tick).await; // nudge a refresh
        });
        self.status = "restoring…".into();
    }

    // ── rename (0059 C1) ──────────────────────────────────────────────────────
    /// Open the rename overlay for the selected session, seeded with its current
    /// display name (the label if set, else the slug).
    fn open_rename(&mut self) {
        let seed = self
            .selected_session()
            .map(|s| (s.name.clone(), s.machine.clone(), display_name(s).to_string()));
        if let Some((session, machine, value)) = seed {
            self.overlay =
                Overlay::RenameSession(RenameForm { session, machine, value, error: None });
        }
    }

    /// Text-field key handling for the switcher rename overlay, via the shared
    /// `rename_key` (same printable→push / Backspace→pop / Esc / Enter logic the
    /// new-session form uses).
    fn key_rename(&mut self, k: KeyEvent) {
        let action = {
            let Overlay::RenameSession(form) = &mut self.overlay else {
                return;
            };
            rename_key(form, k)
        };
        match action {
            RenameAction::None => {}
            RenameAction::Cancel => self.overlay = Overlay::None,
            RenameAction::Submit => self.submit_rename(),
        }
    }

    fn submit_rename(&mut self) {
        let vals = match &self.overlay {
            Overlay::RenameSession(f) => Some((f.session.clone(), f.machine.clone(), f.value.clone())),
            _ => None,
        };
        if let Some((session, machine, value)) = vals {
            self.spawn_set_label(session, machine, value);
        }
    }

    /// Spawn the label mutation; the result arrives as `AppMsg::Labeled`. An empty
    /// value clears the label (the server falls back to the slug), so submit `None`.
    fn spawn_set_label(&self, session: String, machine: String, value: String) {
        let trimmed = value.trim().to_string();
        let label = if trimmed.is_empty() { None } else { Some(trimmed) };
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = rest
                .set_label(&session, label.as_deref(), &machine)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(AppMsg::Labeled(r)).await;
        });
    }

    fn handle_labeled(&mut self, res: Result<(), String>) {
        match res {
            Ok(()) => {
                // Close whichever rename overlay is open (switcher or grid) + refresh.
                // Guard on the overlay *kind*: a slow reply must never clobber a
                // different overlay the user opened meanwhile (e.g. Esc'd the rename,
                // then opened New-session / a delete-confirm). Symmetric with the
                // Err arm and with `handle_created`.
                if matches!(self.overlay, Overlay::RenameSession(_)) {
                    self.overlay = Overlay::None;
                }
                if matches!(self.grid_overlay, GridOverlay::Rename(_)) {
                    self.grid_overlay = GridOverlay::None;
                }
                self.status = "renamed".into();
                self.pending_refresh = true;
            }
            // Keep the open overlay up and surface the server's message.
            Err(e) => {
                if let Overlay::RenameSession(f) = &mut self.overlay {
                    f.error = Some(e);
                } else if let GridOverlay::Rename(f) = &mut self.grid_overlay {
                    f.error = Some(e);
                }
            }
        }
    }

    // ── restorable picker (0059 C5) ──────────────────────────────────────────
    /// Fetch the restorable-session list (for the default machine) and post it back
    /// as `AppMsg::Restorable`, which opens the picker (or reports "nothing to
    /// restore" on an empty/failed fetch).
    fn open_restore_picker(&mut self) {
        let rest = self.rest.clone();
        let tx = self.tx.clone();
        let machine = self.default_machine_name();
        self.status = "loading restorable…".into();
        tokio::spawn(async move {
            let list = rest.restorable(&machine).await.unwrap_or_default();
            let _ = tx.send(AppMsg::Restorable(list)).await;
        });
    }

    fn handle_restorable(&mut self, list: Vec<RestorableSession>) {
        if list.is_empty() {
            self.status = "nothing to restore".into();
            return;
        }
        let n = list.len();
        self.overlay = Overlay::RestorePicker(RestoreForm {
            items: list,
            // Pre-checked: restore is all-or-nothing, so every row comes back.
            selected: vec![true; n],
            cursor: 0,
        });
    }

    fn key_restore_picker(&mut self, k: KeyEvent) {
        let Overlay::RestorePicker(form) = &mut self.overlay else {
            return;
        };
        let n = form.items.len();
        match k.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Down | KeyCode::Char('j') if n > 0 => form.cursor = (form.cursor + 1) % n,
            KeyCode::Up | KeyCode::Char('k') if n > 0 => form.cursor = (form.cursor + n - 1) % n,
            KeyCode::Char(' ') => {
                if let Some(b) = form.selected.get_mut(form.cursor) {
                    *b = !*b;
                }
            }
            // `a` selects all, then restores (Enter/a both restore — see the note
            // in submit_restore_picker).
            KeyCode::Char('a') => {
                form.selected.iter_mut().for_each(|b| *b = true);
                self.submit_restore_picker();
            }
            KeyCode::Enter => self.submit_restore_picker(),
            _ => {}
        }
    }

    fn submit_restore_picker(&mut self) {
        // NOTE: restore is all-or-nothing server-side — `POST /api/sessions/restore`
        // takes no body and `Cmd::Restore` carries no session list, so the per-row
        // checkboxes are a preview/confirmation of what will come back, not a
        // selection. Per-session selective restore would need a new backend
        // endpoint (out of scope here — 0059 C5 is TUI-only).
        self.overlay = Overlay::None;
        self.restore_all();
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
            GridOverlay::Rename(_) => 4,
        };
        match overlay {
            1 => return self.key_palette(k),
            2 => return self.key_menu(k),
            3 => return self.key_grid_newform(k),
            4 => return self.key_grid_rename(k),
            _ => {}
        }
        // Keyboard-scroll mode captures navigation keys for the focused pane.
        if self.scroll_mode {
            return self.key_scroll(k);
        }
        if self.prefix_armed {
            self.prefix_armed = false;
            if self.is_prefix(k) {
                self.send_key_to_active(k); // prefix prefix → literal prefix
                return;
            }
            match k.code {
                KeyCode::Char('d') => self.open_menu(self.active),
                // 0062: the search-first switcher as the grid's pick-a-session
                // view for the focused box (type-to-filter, Enter fills it).
                KeyCode::Char('s') => self.open_switcher_for(self.active),
                KeyCode::Char('[') => self.enter_scroll_mode(), // 0059 C3: scroll mode
                KeyCode::Char('x') => self.kill_focused(),
                KeyCode::Char('g') => self.jump_ready(), // 0018: go to ready session(s)
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

    /// `^A s` (0062): open the full-screen switcher as the grid's
    /// pick-a-session view, scoped to `target` — type-to-search, Enter fills
    /// the box (and `Ctrl+N`'s create lands there too, via `fill_target`),
    /// Esc on an empty query cancels back to the grid. This is the fill-a-box
    /// invocation the switcher's `fill_target` handling always supported; the
    /// chord makes it reachable without clearing the box first.
    fn open_switcher_for(&mut self, target: usize) {
        self.fill_target = Some(target.min(self.panes.len().saturating_sub(1)));
        self.grid_overlay = GridOverlay::None;
        self.prefix_armed = false;
        self.scroll_mode = false;
        self.clear_query();
        self.mode = Mode::Switcher;
        self.pending_refresh = true;
    }

    // ── layout palette ───────────────────────────────────────────────────────
    fn open_palette(&mut self) {
        let cur = Layout::ALL.iter().position(|&l| l == self.layout).unwrap_or(0);
        self.grid_overlay = GridOverlay::Palette(cur);
        self.prefix_armed = false;
        self.scroll_mode = false; // leaving the live pane view exits scroll mode
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
        // Direct-attach (0059 C2): `ccs <session>` boots straight into the grid
        // attached to the resolved session, skipping the action menu. `main.rs`
        // already pre-flighted the query for the exit-code UX; we re-resolve here
        // against the freshly-refreshed list (same data, same pure function) so a
        // late miss falls back to the default menu boot instead of a blank grid.
        if let Some(query) = self.start_attach_query.take() {
            if let Ok((name, machine)) = resolve_attach(&self.sessions, &query) {
                self.fill_box(0, name, machine); // → Grid mode, box 0, no menu
                return;
            }
        }
        match self.sessions.first() {
            Some(first) => {
                let (name, machine) = (first.name.clone(), first.machine.clone());
                self.fill_box(0, name, machine); // → Grid mode, box 0
            }
            None => self.mode = Mode::Grid,
        }
        self.open_menu(0);
    }

    // ── unified action menu (Ctrl-A d / empty box) ───────────────────────────
    fn open_menu(&mut self, target: usize) {
        let target = target.min(self.panes.len().saturating_sub(1));
        let current = self.panes.get(target).and_then(|p| p.as_ref()).map(|p| p.session.clone());
        let selected =
            menu_initial(&self.sessions, &self.grouped_session_order(), current.as_deref());
        self.grid_overlay = GridOverlay::Menu { target, selected, query: String::new() };
        self.prefix_armed = false;
        self.scroll_mode = false; // leaving the live pane view exits scroll mode
    }

    // Search-first menu keys (0062b), mirroring the switcher's `key_list`: bare
    // printables type into the query (so `j`/`k` no longer navigate — arrows
    // do), and every action resolves through the filtered rows at press time.
    fn key_menu(&mut self, k: KeyEvent) {
        let (target, selected, query) = match &self.grid_overlay {
            GridOverlay::Menu { target, selected, query } => (*target, *selected, query.clone()),
            _ => return,
        };
        let can_rename = self.box_has_session(target);
        let set = |app: &mut Self, selected: usize, query: String| {
            app.grid_overlay = GridOverlay::Menu { target, selected, query };
        };
        match (k.code, k.modifiers) {
            // Esc: clear an active query first; on an empty query close the
            // menu — the web sidebar's clear-then-close two-step.
            (KeyCode::Esc, _) => {
                if !query.is_empty() {
                    set(self, 0, String::new());
                } else {
                    self.grid_overlay = GridOverlay::None;
                }
            }
            (KeyCode::Up, _) | (KeyCode::Down, _) => {
                // Wraps over the *selectable* (possibly filtered) rows; headers
                // are skipped, so the cursor can never land on one.
                let len = menu_selectable_len(&self.menu_rows(can_rename, &query));
                if len == 0 {
                    return;
                }
                let delta = if k.code == KeyCode::Up { len - 1 } else { 1 };
                set(self, (selected + delta) % len, query);
            }
            (KeyCode::Enter, _) => {
                match menu_selectable(&self.menu_rows(can_rename, &query), selected) {
                    Some(MenuRow::Session(i)) => {
                        if let Some(s) = self.sessions.get(*i) {
                            let (name, machine) = (s.name.clone(), s.machine.clone());
                            self.grid_overlay = GridOverlay::None;
                            self.fill_box(target, name, machine);
                        }
                    }
                    Some(&MenuRow::Action(item)) => self.activate_menu(target, item),
                    _ => {}
                }
            }
            (KeyCode::Backspace, _) => {
                let mut q = query;
                q.pop();
                set(self, 0, q);
            }
            // Type-to-search: any printable key (bare or shifted) appends to
            // the query and re-ranks immediately, cursor snapping to the top.
            (KeyCode::Char(ch), m)
                if !m.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                let mut q = query;
                q.push(ch);
                set(self, 0, q);
            }
            _ => {}
        }
    }

    /// Whether box `target` currently holds an attached session (gates the menu's
    /// Rename row + the pane-title/statusbar rename resolution).
    fn box_has_session(&self, target: usize) -> bool {
        self.panes.get(target).and_then(|p| p.as_ref()).is_some()
    }

    fn activate_menu(&mut self, target: usize, item: MenuItem) {
        match item {
            // Hand off to the existing centered modals. (Session rows resolve in
            // `key_menu` directly — they carry a `sessions` index, not an item.)
            MenuItem::ChangeLayout => self.open_palette(),
            MenuItem::NewSession => self.open_grid_newform(target),
            MenuItem::RenameSession => self.open_grid_rename(target),
            MenuItem::ClearBox => {
                self.grid_overlay = GridOverlay::None;
                self.clear_box(target);
            }
            MenuItem::Quit => self.should_quit = true,
        }
    }

    /// Open the grid rename overlay for box `target`'s session (0059 C1), seeded
    /// with its current display name. No-op for an empty box.
    fn open_grid_rename(&mut self, target: usize) {
        let Some((session, machine)) =
            self.panes.get(target).and_then(|p| p.as_ref()).map(|p| (p.session.clone(), p.machine.clone()))
        else {
            return;
        };
        let value = self
            .sessions
            .iter()
            .find(|s| s.name == session && s.machine == machine)
            .map(display_name)
            .unwrap_or(session.as_str())
            .to_string();
        self.grid_overlay = GridOverlay::Rename(RenameForm { session, machine, value, error: None });
    }

    fn key_grid_rename(&mut self, k: KeyEvent) {
        let action = {
            let GridOverlay::Rename(form) = &mut self.grid_overlay else {
                return;
            };
            rename_key(form, k)
        };
        match action {
            RenameAction::None => {}
            RenameAction::Cancel => self.grid_overlay = GridOverlay::None,
            RenameAction::Submit => {
                let vals = match &self.grid_overlay {
                    GridOverlay::Rename(f) => {
                        Some((f.session.clone(), f.machine.clone(), f.value.clone()))
                    }
                    _ => None,
                };
                if let Some((session, machine, value)) = vals {
                    self.spawn_set_label(session, machine, value);
                }
            }
        }
    }

    // ── inline new-session form (fills a box on submit) ───────────────────────
    fn open_grid_newform(&mut self, target: usize) {
        if self.tools.is_empty() {
            self.status = "no tools available".into();
            return;
        }
        let form = NewForm::new(self.seed_dir(), self.default_machine_idx());
        let parent = dir_parent_of(&form.dir);
        self.grid_overlay = GridOverlay::NewForm { target, form };
        self.spawn_dir_fetch(parent);
    }

    fn key_grid_newform(&mut self, k: KeyEvent) {
        let tools_len = self.tools.len();
        let machines_len = self.machines.len();
        let (target, action) = match &mut self.grid_overlay {
            GridOverlay::NewForm { target, form } => {
                (*target, newform_key(form, tools_len, machines_len, k))
            }
            _ => return,
        };
        match action {
            NewFormAction::None => {}
            NewFormAction::Cancel => self.grid_overlay = GridOverlay::None,
            NewFormAction::FetchDirs(parent) => self.spawn_dir_fetch(parent),
            NewFormAction::MachineChanged(parent) => {
                self.spawn_dir_fetch(parent);
                self.spawn_tools_fetch();
            }
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

    // ── keyboard scrollback (0059 C3) ────────────────────────────────────────
    /// Enter tmux-shaped scroll mode on the focused pane (`^A [`). A no-op when
    /// the focused box is empty — there's nothing to scroll.
    ///
    /// Refused when the child is in the alternate screen (0069 Part D): that grid
    /// has no history at all, so the mode would pin at offset 0 *and* swallow the
    /// very keys (`PgUp`/`j`/`k`) the child would have scrolled on. Say so and
    /// leave the keys flowing — this deliberately amends [0059] C3's "every mouse
    /// affordance has a keyboard path": here the keyboard path is the child's own.
    fn enter_scroll_mode(&mut self) {
        let Some(p) = self.panes.get(self.active).and_then(|x| x.as_ref()) else {
            return;
        };
        if p.alt_screen() {
            self.set_hint("alt screen: no scrollback (app controls its own view)");
            return;
        }
        self.scroll_mode = true;
    }

    /// Flash a transient note in the grid statusbar (0069 Part D).
    fn set_hint(&mut self, msg: &str) {
        self.hint = Some((msg.to_string(), Instant::now() + HINT_TTL));
    }

    /// The hint text while it is still live (it simply stops rendering once the
    /// TTL passes — the next redraw the 1 s ticker forces takes it off screen).
    fn hint_text(&self) -> Option<&str> {
        self.hint.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(t, _)| t.as_str())
    }

    /// Route a key while the focused pane is in keyboard-scroll mode. tmux-shaped:
    /// `PgUp`/`PgDn` or `Ctrl+U`/`Ctrl+D` page, `k`/`j` line-step, `g`/`G`
    /// top/live, `q`/`Esc` return to live. Nothing here reaches the input encoder;
    /// scroll is visual-only (input still targets the live session). Every other
    /// key is swallowed — the mode is modal until an explicit exit.
    fn key_scroll(&mut self, k: KeyEvent) {
        let page = self.box_size(self.active).1 as isize; // one screen = pane rows
        let Some(p) = self.panes.get_mut(self.active).and_then(|x| x.as_mut()) else {
            self.scroll_mode = false; // the pane vanished — drop back to live
            return;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('G') => {
                p.scroll_to_live();
                self.scroll_mode = false;
            }
            KeyCode::Char('g') => p.scroll(SCROLL_TOP), // jump to the oldest line
            KeyCode::PageUp => p.scroll(page),
            KeyCode::PageDown => p.scroll(-page),
            KeyCode::Char('u') if ctrl => p.scroll(page),
            KeyCode::Char('d') if ctrl => p.scroll(-page),
            KeyCode::Char('k') => p.scroll(1),
            KeyCode::Char('j') => p.scroll(-1),
            _ => {}
        }
    }

    // ── attach / fill / layout ───────────────────────────────────────────────
    fn attach(&mut self) {
        let Some(s) = self.selected_session() else {
            return;
        };
        let (session, machine) = (s.name.clone(), s.machine.clone());
        let target = self.fill_target.take().unwrap_or(0).min(self.panes.len().saturating_sub(1));
        self.fill_box(target, session, machine);
    }

    fn fill_box(&mut self, idx: usize, session: String, machine: String) {
        if idx >= self.panes.len() {
            return;
        }
        // Dedupe: a (machine, session) may live in at most one box (else they
        // fight over the single PTY's width). Same name on different machines is
        // distinct, so both halves of the key matter.
        for (j, slot) in self.panes.iter_mut().enumerate() {
            if j != idx
                && slot.as_ref().is_some_and(|p| p.session == session && p.machine == machine)
            {
                *slot = None;
            }
        }
        let (cols, rows) = self.box_size(idx);
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let (out_tx, out_rx) = mpsc::channel::<WsOut>(1024);
        let url = self.rest.urls().ws(&session, &machine);
        let token = self.rest.token().map(str::to_owned);
        let task = tokio::spawn(ws::run(url, token, id, cols, rows, out_rx, self.tx.clone()));

        self.remember(&session);
        self.panes[idx] = Some(Pane::new(id, session, machine, cols, rows, out_tx, task));
        self.refresh_pane_accents();
        self.active = idx;
        self.mode = Mode::Grid;
        self.prefix_armed = false;
        self.scroll_mode = false; // a fresh attach starts live
    }

    /// Sync each mounted box's border accent from the current session list's
    /// web-set colour marks (proposal 0029 / 0059 C4). Display-only: a colour set
    /// on the phone shows on the next poll; an unmarked/unknown token clears it.
    fn refresh_pane_accents(&mut self) {
        for slot in self.panes.iter_mut() {
            let Some(p) = slot.as_mut() else { continue };
            let accent = self
                .sessions
                .iter()
                .find(|s| s.name == p.session && s.machine == p.machine)
                .and_then(|s| ui::util::color_token_to_color(s.color.as_deref()));
            p.set_accent(accent);
        }
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
            // Drop any grid modal (`jump_ready` parity): an orphaned menu would
            // otherwise resurrect — stale query, out-of-range target — the next
            // time a `fill_box` re-enters Grid mode.
            self.grid_overlay = GridOverlay::None;
            self.clear_query(); // a fresh switcher entry starts unfiltered
            self.set_layout(Layout::Single);
            self.active = 0;
            self.pending_refresh = true;
        }
    }

    fn handle_pane(&mut self, id: u64, msg: PaneMsg) {
        let active = self.active;
        for (i, slot) in self.panes.iter_mut().enumerate() {
            if let Some(p) = slot {
                if p.id == id {
                    match msg {
                        PaneMsg::Bytes(b) => p.process(&b),
                        PaneMsg::State(s) => p.set_state(s),
                    }
                    // The child just went fullscreen under an open scroll mode:
                    // drop out of it rather than sit there swallowing keys the
                    // child could act on (0069 Part D, the entry guard's twin).
                    if i == active && self.scroll_mode && p.alt_screen() {
                        self.scroll_mode = false;
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

    /// Build the render view for a new-session form (shared by both overlays).
    /// In hub mode the machine row is a picker over the connected agents; in
    /// direct mode it's a read-only label naming this single box (`pickable`
    /// false), shown only once we know the name.
    fn new_session_view<'a>(&'a self, form: &'a NewForm) -> ui::overlay::NewSessionView<'a> {
        let tool = self.tools.get(form.tool_idx).map(|t| t.prefix.as_str()).unwrap_or("-");
        let (machine, machine_online, pickable) = if self.hub_mode {
            match self.machines.get(form.machine_idx) {
                Some(m) => (Some(m.machine.as_str()), m.online, true),
                None => (None, true, true),
            }
        } else if !self.self_machine.is_empty() {
            (Some(self.self_machine.as_str()), true, false)
        } else {
            (None, true, false)
        };
        ui::overlay::NewSessionView {
            tool,
            machine,
            machine_online,
            machine_pickable: pickable,
            name: &form.name,
            dir: &form.dir,
            focus: form.field,
            candidates: &form.dir_cands,
            cand_sel: form.dir_sel,
            error: form.error.as_deref(),
            skip_permissions: form.skip_permissions,
        }
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
                        ui::overlay::new_session(f, &self.new_session_view(form));
                    }
                    Overlay::RenameSession(form) => {
                        ui::overlay::rename_session(
                            f,
                            &ui::overlay::RenameView {
                                current: &form.session,
                                value: &form.value,
                                error: form.error.as_deref(),
                            },
                        );
                    }
                    Overlay::RestorePicker(form) => {
                        ui::overlay::restore_picker(
                            f,
                            &ui::overlay::RestoreView {
                                items: &form.items,
                                selected: &form.selected,
                                cursor: form.cursor,
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
                    &self.sessions,
                    self.active,
                    &self.prefix_label(),
                    self.prefix_armed,
                    self.scroll_mode,
                    self.toast_text().as_deref(),
                    self.hint_text(),
                );
                match &self.grid_overlay {
                    GridOverlay::None => {}
                    GridOverlay::Palette(hi) => ui::overlay::layout_palette(f, *hi),
                    GridOverlay::Menu { target, selected, query } => {
                        let rows = self.menu_rows(self.box_has_session(*target), query);
                        // Machine chips only while filtering in multi-machine hub
                        // mode (headers cover the resting state) — precomputed
                        // here so the overlay stays App-free. Parallel to
                        // `sessions`; an empty string suppresses the chip.
                        let chips: Vec<String> =
                            if !query.trim().is_empty() && self.multi_machine() {
                                self.sessions
                                    .iter()
                                    .map(|s| {
                                        if s.machine.is_empty() {
                                            String::new()
                                        } else {
                                            self.machine_label(&s.machine)
                                        }
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                        ui::overlay::grid_menu(
                            f,
                            &ui::overlay::MenuView {
                                rows: &rows,
                                sessions: &self.sessions,
                                query,
                                selected_row: menu_selected_row(&rows, *selected),
                                chips: &chips,
                                box_num: *target + 1,
                                box_count: self.panes.len(),
                            },
                        )
                    }
                    GridOverlay::NewForm { form, .. } => {
                        ui::overlay::new_session(f, &self.new_session_view(form));
                    }
                    GridOverlay::Rename(form) => ui::overlay::rename_session(
                        f,
                        &ui::overlay::RenameView {
                            current: &form.session,
                            value: &form.value,
                            error: form.error.as_deref(),
                        },
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
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the switcher list is being filtered (non-empty query).
    pub fn filtering(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Hub mode with more than one connected machine — when the switcher groups
    /// under per-machine headers at rest and chips rows while filtering
    /// (proposal 0062 Part B). Direct mode and single-machine hubs render
    /// exactly as before.
    pub fn multi_machine(&self) -> bool {
        self.hub_mode && self.machines.len() > 1
    }

    /// The display name for a machine id: its hostname when known, the id
    /// otherwise, "this machine" for the empty id (the web sidebar's fallback).
    pub fn machine_label(&self, id: &str) -> String {
        match self.machines.iter().find(|m| m.machine == id) {
            Some(m) if !m.hostname.is_empty() => m.hostname.clone(),
            _ if !id.is_empty() => id.to_string(),
            _ => "this machine".into(),
        }
    }

    fn machine_online(&self, id: &str) -> bool {
        self.machines.iter().find(|m| m.machine == id).is_none_or(|m| m.online)
    }

    /// Session indices in the resting display order: identity for a single
    /// machine / direct agent, grouped by machine otherwise. Shared by the
    /// switcher's resting list and the grid action menu (0062b) so both
    /// surfaces agree on grouping — and independent of either query, so a
    /// stale switcher filter can't leak into the menu.
    fn grouped_session_order(&self) -> Vec<usize> {
        if !self.multi_machine() {
            return (0..self.sessions.len()).collect();
        }
        // Group by machine: known machines in `/api/machines` order, then
        // any stragglers in first-appearance order. Name order inside a
        // group rides on `sessions` being name-sorted.
        let mut order: Vec<&str> = self.machines.iter().map(|m| m.machine.as_str()).collect();
        for s in &self.sessions {
            if !order.contains(&s.machine.as_str()) {
                order.push(&s.machine);
            }
        }
        let mut out = Vec::with_capacity(self.sessions.len());
        for m in order {
            out.extend(
                self.sessions.iter().enumerate().filter(|(_, s)| s.machine == m).map(|(i, _)| i),
            );
        }
        out
    }

    /// Session indices in display order: ranked (name > path > meta, the web's
    /// 0028 tiers) while filtering; resting order (name-sorted, grouped by
    /// machine when `multi_machine`) otherwise. `selected` indexes THIS list.
    pub fn visible_sessions(&self) -> Vec<usize> {
        let q = self.query.trim();
        if q.is_empty() {
            self.grouped_session_order()
        } else {
            let mut scored: Vec<(i64, usize)> = self
                .sessions
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    score_session(s, &self.machine_label(&s.machine), q).map(|sc| (sc, i))
                })
                .collect();
            // Stable sort: equal scores keep the resting (name) order.
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().map(|(_, i)| i).collect()
        }
    }

    /// The switcher's display rows: per-machine headers interleaved at group
    /// boundaries at rest in multi-machine hub mode; the flat (possibly ranked)
    /// session list otherwise. Headers are non-selectable — the cursor lives in
    /// `visible_sessions()`, whose order matches this list's `Session` rows.
    pub fn switcher_rows(&self) -> Vec<SwitcherRow> {
        let vis = self.visible_sessions();
        if self.filtering() || !self.multi_machine() {
            return vis.into_iter().map(SwitcherRow::Session).collect();
        }
        let mut rows = Vec::with_capacity(vis.len() + self.machines.len());
        let mut cur: Option<&str> = None;
        for i in vis {
            let m = self.sessions[i].machine.as_str();
            if cur != Some(m) {
                rows.push(SwitcherRow::Header {
                    label: self.machine_label(m),
                    online: self.machine_online(m),
                });
                cur = Some(m);
            }
            rows.push(SwitcherRow::Session(i));
        }
        rows
    }

    /// The selected session, resolved through the visible-row indirection —
    /// the only way switcher actions (attach/kill/rename/detail footer) may
    /// address a session, so a filtered-out row can never be acted on.
    pub fn selected_session(&self) -> Option<&SessionInfo> {
        self.visible_sessions().get(self.selected).map(|&i| &self.sessions[i])
    }

    /// The grid action menu's display rows (0062b). Resting (empty/whitespace
    /// `query`): the two create-ish actions, the sessions in the switcher's
    /// grouped order (headers interleaved only in multi-machine hub mode), then
    /// the tail actions — with a single machine this is exactly the pre-0062b
    /// structure, so session `i` still sits at selectable index `2 + i`.
    /// Filtering: no headers; sessions score through `score_session` (tiered)
    /// and actions through their alias terms (untiered), then one stable sort
    /// interleaves them purely by score — web-sidebar ranking parity.
    fn menu_rows(&self, can_rename: bool, query: &str) -> Vec<MenuRow> {
        let q = query.trim();
        let tail = |rows: &mut Vec<MenuRow>| {
            if can_rename {
                rows.push(MenuRow::Action(MenuItem::RenameSession));
            }
            rows.push(MenuRow::Action(MenuItem::ClearBox));
            rows.push(MenuRow::Action(MenuItem::Quit));
        };
        if q.is_empty() {
            let mut rows = Vec::with_capacity(self.sessions.len() + self.machines.len() + 5);
            rows.push(MenuRow::Action(MenuItem::ChangeLayout));
            rows.push(MenuRow::Action(MenuItem::NewSession));
            let headers = self.multi_machine();
            let mut cur: Option<&str> = None;
            for i in self.grouped_session_order() {
                let m = self.sessions[i].machine.as_str();
                if headers && cur != Some(m) {
                    rows.push(MenuRow::Header {
                        label: self.machine_label(m),
                        online: self.machine_online(m),
                    });
                    cur = Some(m);
                }
                rows.push(MenuRow::Session(i));
            }
            tail(&mut rows);
            rows
        } else {
            // Candidates in resting selectable order, so the stable sort keeps
            // that relative order for equal scores. Accepted divergence from the
            // web: its baseItems put New session first, so an exact tie between
            // the two create-ish actions (e.g. the query "n") orders them
            // oppositely there — the 0062b spec pins OUR resting order instead.
            let mut cand: Vec<(i64, MenuRow)> = Vec::new();
            let action = |cand: &mut Vec<(i64, MenuRow)>, it: MenuItem| {
                if let Some(sc) = action_terms(it).iter().filter_map(|t| fuzzy_score_web(q, t)).max()
                {
                    cand.push((sc, MenuRow::Action(it)));
                }
            };
            action(&mut cand, MenuItem::ChangeLayout);
            action(&mut cand, MenuItem::NewSession);
            for i in self.grouped_session_order() {
                let s = &self.sessions[i];
                if let Some(sc) = score_session(s, &self.machine_label(&s.machine), q) {
                    cand.push((sc, MenuRow::Session(i)));
                }
            }
            if can_rename {
                action(&mut cand, MenuItem::RenameSession);
            }
            action(&mut cand, MenuItem::ClearBox);
            action(&mut cand, MenuItem::Quit);
            cand.sort_by(|a, b| b.0.cmp(&a.0)); // stable: ties keep resting order
            cand.into_iter().map(|(_, r)| r).collect()
        }
    }
}

/// One wheel step over `p`, routed by what the child is doing (proposal 0069).
/// Precedence, highest first:
///
/// 1. **already scrolled back locally** — the wheel keeps walking the pane's own
///    history until it returns to live, whatever mode the child is in;
/// 2. **the child captures the mouse** (0069 A) — forward a mouse report, so
///    Claude's fullscreen renderer / `htop` / `lazygit` scroll their own view;
/// 3. **alt screen without mouse capture** (0069 B) — xterm "alternate scroll":
///    `MOUSE_STEP` arrow keys, which is what `less` and `vim` navigate on;
/// 4. **otherwise** — today's local scrollback, unchanged.
///
/// `rect` is the pane's content rect; the report carries **pane-local, 1-based**
/// coordinates so the child places the wheel inside its own screen, not the grid's.
fn wheel(p: &mut Pane, up: bool, rect: Rect, col: u16, row: u16, mods: KeyModifiers) {
    let step = if up { MOUSE_STEP } else { -MOUSE_STEP };
    if p.scroll_offset() > 0 {
        p.scroll(step);
    } else if p.mouse_mode() {
        // Clamped into the rect: the wheel may land on the box border, which is
        // ours, not the child's.
        let c = col.saturating_sub(rect.x).min(rect.width.saturating_sub(1)) + 1;
        let r = row.saturating_sub(rect.y).min(rect.height.saturating_sub(1)) + 1;
        p.send_input(input::encode_wheel(up, c, r, mods, p.sgr_mouse()));
    } else if p.alt_screen() && p.alternate_scroll() {
        let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
        if let Some(one) = input::encode(key, p.application_cursor()) {
            p.send_input(one.repeat(MOUSE_STEP as usize));
        }
    } else {
        p.scroll(step);
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
        let rest = Rest::new("http://127.0.0.1:9", false, None).unwrap();
        let mut a = App::new(rest, Config::default());
        a.sessions = sessions;
        a.status = status.into();
        a
    }

    /// Like `test_fixture`, but in hub mode with a machine list — for the
    /// switcher's multi-machine grouping/chip render tests (proposal 0062 B).
    pub fn test_fixture_hub(
        sessions: Vec<SessionInfo>,
        machines: Vec<cc_screen_protocol::MachineInfo>,
        status: &str,
    ) -> Self {
        let mut a = Self::test_fixture(sessions, status);
        a.hub_mode = true;
        a.machines = machines;
        a
    }

    /// Set the switcher search query directly (render tests drive state, not keys).
    pub fn test_set_query(&mut self, q: &str) {
        self.query = q.into();
    }

    /// Set the switcher cursor directly (an index into `visible_sessions()`).
    pub fn test_set_selected(&mut self, sel: usize) {
        self.selected = sel;
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
            last_input_at: 0,
            busy_since: 0,
            busy_until: 0,
            preview: String::new(),
            waiting: false,
            skip_permissions: None,
            cwd: String::new(),
            machine: String::new(),
            headline: None,
            detail: None,
            color: None,
            label: None,
        }
    }

    /// Like `sess`, but pins the session's machine (for hub / multi-machine cases).
    fn sess_on(name: &str, machine: &str) -> SessionInfo {
        let mut s = sess(name);
        s.machine = machine.into();
        s
    }

    #[test]
    fn resolve_attach_exact_name() {
        let list = vec![sess("alpha"), sess("beta")];
        assert_eq!(resolve_attach(&list, "alpha"), Ok(("alpha".into(), String::new())));
    }

    #[test]
    fn resolve_attach_machine_slash_name() {
        // Same name on two machines: the bare name is ambiguous, but the
        // machine/name form disambiguates to one.
        let list = vec![sess_on("web", "boxA"), sess_on("web", "boxB")];
        assert_eq!(resolve_attach(&list, "boxB/web"), Ok(("web".into(), "boxB".into())));
        assert!(matches!(resolve_attach(&list, "web"), Err(AttachError::Ambiguous(_))));
    }

    #[test]
    fn resolve_attach_unique_prefix() {
        let list = vec![sess("alpha"), sess("beta")];
        // "al" is a unique prefix of alpha (not a prefix of beta).
        assert_eq!(resolve_attach(&list, "al"), Ok(("alpha".into(), String::new())));
    }

    #[test]
    fn resolve_attach_unique_fuzzy() {
        let list = vec![sess("alpha"), sess("gamma")];
        // "aph" is neither a prefix nor a substring, but is a subsequence of alpha
        // (a·l·p·h·a) and of nothing else.
        assert_eq!(resolve_attach(&list, "aph"), Ok(("alpha".into(), String::new())));
    }

    #[test]
    fn resolve_attach_ambiguous_lists_candidates() {
        // "a" prefixes both alpha and apex → ambiguous, with both labelled.
        let list = vec![sess("alpha"), sess("apex")];
        match resolve_attach(&list, "a") {
            Err(AttachError::Ambiguous(c)) => {
                assert!(c.contains(&"alpha".to_string()) && c.contains(&"apex".to_string()), "{c:?}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_attach_missing_is_not_found() {
        let list = vec![sess("alpha"), sess("beta")];
        assert_eq!(resolve_attach(&list, "zzz"), Err(AttachError::NotFound));
        // Even an empty list is NotFound, never a panic.
        assert_eq!(resolve_attach(&[], "alpha"), Err(AttachError::NotFound));
    }

    #[test]
    fn resolve_attach_exact_name_beats_prefix_of_another() {
        // Exact "al" wins outright even though it also prefixes "album".
        let list = vec![sess("al"), sess("album")];
        assert_eq!(resolve_attach(&list, "al"), Ok(("al".into(), String::new())));
    }

    #[test]
    fn display_name_prefers_label_then_short() {
        // 0059 C1: label wins when set + non-empty; otherwise fall back to `short`.
        let mut s = sess("claude-abc");
        assert_eq!(display_name(&s), "claude-abc"); // absent → slug
        s.label = Some("My Feature".into());
        assert_eq!(display_name(&s), "My Feature"); // set → label
        s.label = Some(String::new());
        assert_eq!(display_name(&s), "claude-abc"); // empty → slug
        s.label = None;
        assert_eq!(display_name(&s), "claude-abc"); // cleared → slug
    }

    #[test]
    fn menu_rows_resting_order_and_length() {
        let app = App::test_fixture(vec![sess("a"), sess("b")], "");
        let rows = app.menu_rows(false, "");
        assert_eq!(rows.len(), 6); // 2 sessions + 4 actions, no headers direct
        assert!(matches!(rows[0], MenuRow::Action(MenuItem::ChangeLayout)));
        assert!(matches!(rows[1], MenuRow::Action(MenuItem::NewSession)));
        assert!(matches!(rows[2], MenuRow::Session(0)));
        assert!(matches!(rows[3], MenuRow::Session(1)));
        assert!(matches!(rows[4], MenuRow::Action(MenuItem::ClearBox)));
        assert!(matches!(rows[5], MenuRow::Action(MenuItem::Quit)));
        // 0059 C1: a rename-able box inserts a Rename row between the sessions
        // and Clear this box.
        let rows = app.menu_rows(true, "");
        assert_eq!(rows.len(), 7);
        assert!(matches!(rows[4], MenuRow::Action(MenuItem::RenameSession)));
        // A whitespace-only query is still the resting list.
        assert_eq!(app.menu_rows(false, "  ").len(), 6);
        // No sessions still yields the four action rows.
        let empty = App::test_fixture(vec![], "");
        assert_eq!(empty.menu_rows(false, "").len(), 4);
    }

    #[test]
    fn menu_rows_headers_only_at_rest_in_multi_machine_mode() {
        let app = App::test_fixture_hub(
            vec![sess_on("alpha", "boxA"), sess_on("bravo", "boxB")],
            vec![machine("boxA", "hostA", true), machine("boxB", "hostB", false)],
            "",
        );
        // Resting: headers interleave at group boundaries; they are not
        // selectable, so session i still sits at selectable index 2 + i.
        let rows = app.menu_rows(false, "");
        assert_eq!(rows.len(), 8); // 2 actions + 2 headers + 2 sessions + 2 actions
        assert!(matches!(&rows[2], MenuRow::Header { label, online } if label == "hostA" && *online));
        assert!(matches!(rows[3], MenuRow::Session(0)));
        assert!(matches!(&rows[4], MenuRow::Header { label, online } if label == "hostB" && !online));
        assert!(matches!(menu_selectable(&rows, 2), Some(MenuRow::Session(0))));
        assert!(matches!(menu_selectable(&rows, 3), Some(MenuRow::Session(1))));
        assert_eq!(menu_selectable_len(&rows), 6);
        assert_eq!(menu_selected_row(&rows, 2), Some(3)); // skips the header rows
        // Filtering: NO headers, non-matching rows dropped.
        let rows = app.menu_rows(false, "bravo");
        assert!(!rows.iter().any(|r| matches!(r, MenuRow::Header { .. })));
        assert!(matches!(rows[0], MenuRow::Session(1)));
    }

    #[test]
    fn menu_rows_rank_name_tier_sessions_above_action_aliases() {
        // "split" is an alias of Change layout (web ACTION_TERMS parity), but a
        // NAME-tier session hit must still outrank it — actions score with no
        // tier base, exactly like the web sidebar.
        let app = App::test_fixture(vec![sess("split-api"), sess("other")], "");
        let rows = app.menu_rows(false, "split");
        assert!(matches!(rows[0], MenuRow::Session(0)), "name hit first");
        assert!(
            rows.iter().any(|r| matches!(r, MenuRow::Action(MenuItem::ChangeLayout))),
            "the aliased action still surfaces"
        );
        // A query matching only an alias surfaces just that action.
        let rows = app.menu_rows(false, "tile");
        assert!(matches!(rows[0], MenuRow::Action(MenuItem::ChangeLayout)));
        assert!(!rows.iter().any(|r| matches!(r, MenuRow::Session(_))));
        // Rename's aliases only rank when the row exists (can_rename).
        assert!(app.menu_rows(false, "rename").is_empty());
        assert!(
            matches!(app.menu_rows(true, "rename")[0], MenuRow::Action(MenuItem::RenameSession))
        );
    }

    #[test]
    fn menu_rows_rendered_labels_are_always_searchable() {
        // Web invariant ("The label is matched too"): the label a user is
        // reading must be an alias. The TUI renders "Change layout" where the
        // web says "New layout", so it carries both — typing the on-screen
        // label must never hide the row.
        let app = App::test_fixture(vec![], "");
        for (label, item) in [
            ("change layout", MenuItem::ChangeLayout),
            ("new session", MenuItem::NewSession),
            ("clear this box", MenuItem::ClearBox),
            ("quit ccs", MenuItem::Quit),
        ] {
            let rows = app.menu_rows(item == MenuItem::RenameSession, label);
            assert!(
                rows.iter().any(|r| matches!(r, MenuRow::Action(i) if *i == item)),
                "label {label:?} must surface its own row"
            );
        }
        let rows = app.menu_rows(true, "rename session");
        assert!(rows.iter().any(|r| matches!(r, MenuRow::Action(MenuItem::RenameSession))));
        // The prefix a user types first ("chan…") already matches.
        assert!(
            matches!(app.menu_rows(false, "chan")[0], MenuRow::Action(MenuItem::ChangeLayout))
        );
    }

    #[test]
    fn menu_initial_prefers_current_then_first_then_new() {
        let list = vec![sess("a"), sess("b"), sess("c")];
        let order = [0usize, 1, 2];
        assert_eq!(menu_initial(&list, &order, Some("b")), 3); // 2 + index 1
        assert_eq!(menu_initial(&list, &order, Some("missing")), 2); // falls back to first session
        assert_eq!(menu_initial(&list, &order, None), 2); // first session
        assert_eq!(menu_initial(&[], &[], None), 1); // New session when there are none
        // A grouped (reordered) display order maps through: "b" shown first.
        assert_eq!(menu_initial(&list, &[1, 0, 2], Some("b")), 2);
    }

    #[test]
    fn key_menu_types_into_query_and_jk_are_dead() {
        let mut a = App::test_fixture(vec![sess("alpha"), sess("beta")], "");
        a.grid_overlay = GridOverlay::Menu { target: 0, selected: 2, query: String::new() };
        // Former navigation letters now type into the query, cursor snapping to
        // the top match; nothing activates.
        for c in ['j', 'k'] {
            a.key_menu(key(KeyCode::Char(c)));
        }
        match &a.grid_overlay {
            GridOverlay::Menu { selected, query, .. } => {
                assert_eq!(query, "jk");
                assert_eq!(*selected, 0, "typing snaps the cursor to the top");
            }
            _ => panic!("menu closed by typing"),
        }
        // Backspace pops one char.
        a.key_menu(key(KeyCode::Backspace));
        assert!(matches!(&a.grid_overlay, GridOverlay::Menu { query, .. } if query == "j"));
        // Esc two-step: first clears the query (menu stays open)…
        a.key_menu(key(KeyCode::Esc));
        assert!(
            matches!(&a.grid_overlay, GridOverlay::Menu { query, .. } if query.is_empty()),
            "Esc with a query clears it, keeping the menu open"
        );
        // …then closes the menu.
        a.key_menu(key(KeyCode::Esc));
        assert!(matches!(a.grid_overlay, GridOverlay::None));
    }

    #[test]
    fn key_menu_navigation_wraps_over_the_selectable_rows() {
        // 1 session, no rename → 5 selectable rows [layout, new, s0, clear, quit].
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.grid_overlay = GridOverlay::Menu { target: 0, selected: 0, query: String::new() };
        a.key_menu(key(KeyCode::Up));
        assert!(
            matches!(&a.grid_overlay, GridOverlay::Menu { selected: 4, .. }),
            "up from the top wraps to Quit"
        );
        a.key_menu(key(KeyCode::Down));
        assert!(
            matches!(&a.grid_overlay, GridOverlay::Menu { selected: 0, .. }),
            "down from the bottom wraps to the top"
        );
    }

    #[test]
    fn after_box_removed_clears_an_orphaned_menu() {
        // A menu still open when the LAST box empties (session ended
        // server-side, refresh auto-detach) must not resurrect — stale query,
        // out-of-range target — on the next grid entry (`jump_ready` parity).
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.grid_overlay = GridOverlay::Menu { target: 3, selected: 0, query: "stale".into() };
        a.after_box_removed();
        assert!(matches!(a.mode, Mode::Switcher));
        assert!(matches!(a.grid_overlay, GridOverlay::None), "the orphaned menu must be dropped");
    }

    #[test]
    fn key_menu_enter_resolves_through_the_filtered_rows() {
        // With the query "exit" the only surviving row is Quit (alias hit), so
        // Enter at selectable index 0 must quit — proof the press-time rows are
        // consulted, not the resting indices (where 0 = Change layout).
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.grid_overlay = GridOverlay::Menu { target: 0, selected: 0, query: "exit".into() };
        a.key_menu(key(KeyCode::Enter));
        assert!(a.should_quit(), "Enter resolved the filtered Quit row");
        // Zero matches: Enter is a no-op (nothing selectable).
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.grid_overlay = GridOverlay::Menu { target: 0, selected: 0, query: "zzzz".into() };
        a.key_menu(key(KeyCode::Enter));
        assert!(!a.should_quit());
        assert!(matches!(a.grid_overlay, GridOverlay::Menu { .. }));
    }

    fn form() -> NewForm {
        NewForm::new(String::new(), 0)
    }
    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    fn entry(name: &str) -> DirEntry {
        DirEntry { name: name.into(), path: format!("/home/u/{name}") }
    }

    #[test]
    fn newform_key_edits_submits_and_cancels() {
        let mut f = form(); // starts on the name field
        assert!(matches!(newform_key(&mut f, 3, 0, key(KeyCode::Char('x'))), NewFormAction::None));
        assert_eq!(f.name, "x");
        // Tab: name → dir (no machine field with machines_len == 0).
        newform_key(&mut f, 3, 0, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Dir);
        // Typing a separator changes the parent dir → triggers a fetch.
        assert!(matches!(newform_key(&mut f, 3, 0, key(KeyCode::Char('/'))), NewFormAction::FetchDirs(_)));
        assert_eq!(f.dir, "/");
        // Submit / cancel from the dir field (no candidate highlighted).
        assert!(matches!(newform_key(&mut f, 3, 0, key(KeyCode::Enter)), NewFormAction::Submit));
        assert!(matches!(newform_key(&mut f, 3, 0, key(KeyCode::Esc)), NewFormAction::Cancel));
    }

    #[test]
    fn newform_tab_cycles_fields_with_and_without_machine() {
        let mut f = form();
        f.field = FormField::Tool;
        // No machines: Tool → Name → Dir → SkipPermissions → Tool.
        newform_key(&mut f, 3, 0, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Name);
        newform_key(&mut f, 3, 0, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Dir);
        newform_key(&mut f, 3, 0, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::SkipPermissions);
        newform_key(&mut f, 3, 0, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Tool);
        // With machines: Tool → Machine → Name.
        newform_key(&mut f, 3, 2, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Machine);
        newform_key(&mut f, 3, 2, key(KeyCode::Tab));
        assert_eq!(f.field, FormField::Name);
    }

    #[test]
    fn newform_policy_toggles() {
        let mut f = form();
        // Default: skip-permissions on (0014 retired the hub-control toggle).
        assert!(f.skip_permissions);
        // Space flips the focused toggle; ←/→ do too.
        f.field = FormField::SkipPermissions;
        newform_key(&mut f, 3, 0, key(KeyCode::Char(' ')));
        assert!(!f.skip_permissions, "space toggles skip off");
        newform_key(&mut f, 3, 0, key(KeyCode::Right));
        assert!(f.skip_permissions, "→ toggles skip back on");
        newform_key(&mut f, 3, 0, key(KeyCode::Left));
        assert!(!f.skip_permissions, "← toggles skip off");
        // Enter still submits from a toggle field.
        assert!(matches!(newform_key(&mut f, 3, 0, key(KeyCode::Enter)), NewFormAction::Submit));
    }

    #[test]
    fn newform_arrows_cycle_focused_selector_only() {
        let mut f = form();
        // Tool field: ←/→ cycles the tool.
        f.field = FormField::Tool;
        newform_key(&mut f, 3, 2, key(KeyCode::Right));
        assert_eq!(f.tool_idx, 1);
        assert_eq!(f.machine_idx, 0); // machine untouched
        newform_key(&mut f, 3, 2, key(KeyCode::Left));
        assert_eq!(f.tool_idx, 0);
        // Machine field: ←/→ cycles the machine and re-fetches dirs + tools for it.
        f.field = FormField::Machine;
        assert!(matches!(
            newform_key(&mut f, 3, 2, key(KeyCode::Right)),
            NewFormAction::MachineChanged(_)
        ));
        assert_eq!(f.machine_idx, 1);
        assert_eq!(f.tool_idx, 0); // tool untouched
    }

    #[test]
    fn dir_candidate_navigation_and_accept() {
        let mut f = form();
        f.field = FormField::Dir;
        f.dir = "/home/u/".into();
        f.dir_parent = "/home/u".into();
        f.dir_raw = vec![entry("dev"), entry("docs"), entry("downloads")];
        refilter_dirs(&mut f);
        assert_eq!(f.dir_cands.len(), 3);
        assert_eq!(f.dir_sel, None); // Enter would submit here
        // ↓ enters the list; Enter then accepts instead of submitting.
        newform_key(&mut f, 1, 0, key(KeyCode::Down));
        assert_eq!(f.dir_sel, Some(0));
        let acted = newform_key(&mut f, 1, 0, key(KeyCode::Enter));
        assert!(matches!(acted, NewFormAction::FetchDirs(_)));
        assert_eq!(f.dir, "/home/u/dev/"); // completed + trailing slash to drill in
        assert_eq!(f.dir_sel, None);
    }

    #[test]
    fn dir_filter_ranks_prefix_first_and_hides_dotfiles() {
        let mut f = form();
        f.dir = "/home/u/do".into();
        f.dir_parent = "/home/u".into();
        f.dir_raw = vec![entry("dev"), entry("docs"), entry("downloads"), entry(".cache")];
        refilter_dirs(&mut f);
        // "do" matches docs + downloads (prefix), not dev; .cache hidden.
        let names: Vec<&str> = f.dir_cands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "downloads"]);
    }

    #[test]
    fn dir_parent_and_partial_split() {
        assert_eq!(dir_parent_of("/home/u/dev"), "/home/u");
        assert_eq!(dir_partial_of("/home/u/dev"), "dev");
        assert_eq!(dir_parent_of("/home/u/"), "/home/u");
        assert_eq!(dir_partial_of("/home/u/"), "");
        assert_eq!(dir_parent_of("/abc"), "/");
        assert_eq!(dir_parent_of(""), "");
    }

    #[test]
    fn fuzzy_scores_prefix_over_substring_over_subsequence() {
        let prefix = fuzzy_score("development", "dev");
        let substring = fuzzy_score("my-dev", "dev");
        let subseq = fuzzy_score("daemon-vault", "dev");
        assert!(prefix > substring);
        assert!(substring > subseq);
        assert!(subseq.is_some());
        assert_eq!(fuzzy_score("docs", "xyz"), None);
        assert!(fuzzy_score("anything", "").is_some());
    }

    // ── switcher search (proposal 0062) ──────────────────────────────────────

    fn machine(id: &str, hostname: &str, online: bool) -> MachineInfo {
        MachineInfo { machine: id.into(), hostname: hostname.into(), online }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Shared ranking vectors with the web scorer (`frontend/src/util.ts`
    /// `fuzzyScore`): same inputs, same relative order — the 0062 parity
    /// contract. Absolute values are asserted for the bonus arithmetic.
    #[test]
    fn fuzzy_score_web_matches_the_web_scorer() {
        // No match / empty query.
        assert_eq!(fuzzy_score_web("xyz", "docs"), None);
        assert_eq!(fuzzy_score_web("", "anything"), Some(0));
        assert_eq!(fuzzy_score_web("a", ""), None);
        // Head-of-string hit: +2 (char) +6 (head) +12 (word start) = 20.
        assert_eq!(fuzzy_score_web("d", "dev"), Some(20));
        // Word-start hit after a separator: +2 +12 = 14.
        assert_eq!(fuzzy_score_web("d", "my-dev"), Some(14));
        // Mid-word hit: just +2.
        assert_eq!(fuzzy_score_web("e", "dev"), Some(2));
        // Contiguous run: "de" on "dev" = 20 + (2 + 4·1) = 26.
        assert_eq!(fuzzy_score_web("de", "dev"), Some(26));
        // Case-insensitive.
        assert_eq!(fuzzy_score_web("DE", "dev"), fuzzy_score_web("de", "dev"));
        // A contiguous prefix run beats a scattered mid-word subsequence.
        assert!(fuzzy_score_web("dev", "development") > fuzzy_score_web("dev", "widevine"));
        // …and (web parity, deliberately) word-start bonuses can make up for a
        // broken run: "dev" ties on "development" (d@head + contiguous e,v) and
        // "daemon-vault" (d@head, scattered e, v@word-start).
        assert_eq!(fuzzy_score_web("dev", "development"), Some(36));
        assert_eq!(fuzzy_score_web("dev", "daemon-vault"), Some(36));
    }

    #[test]
    fn score_session_tiers_name_over_path_over_meta() {
        let mut name_hit = sess("apollo");
        name_hit.cwd = "/x/y".into();
        let mut path_hit = sess("zzz");
        path_hit.cwd = "/home/apollo".into();
        let mut meta_hit = sess("yyy");
        meta_hit.headline = Some("fixing apollo tests".into());
        let n = score_session(&name_hit, "", "apo").unwrap();
        let p = score_session(&path_hit, "", "apo").unwrap();
        let m = score_session(&meta_hit, "", "apo").unwrap();
        assert!(n > p && p > m, "name {n} > path {p} > meta {m}");
        assert!(n >= NAME_TIER && p >= PATH_TIER && p < NAME_TIER && m < PATH_TIER);
        // A renamed session is findable by its label at NAME tier.
        let mut labelled = sess("slug-1");
        labelled.label = Some("apollo work".into());
        assert!(score_session(&labelled, "", "apo").unwrap() >= NAME_TIER);
        // The machine is searchable by id and by resolved hostname (META tier).
        let on_box = sess_on("web", "boxA");
        assert!(score_session(&on_box, "pine", "pine").is_some());
        assert!(score_session(&on_box, "pine", "boxa").is_some());
        // …but the EMPTY id (direct agent) must not surface via its "this
        // machine" display placeholder — the web scores the raw id only, so
        // "mac"/"machine" would otherwise match every direct-mode session.
        assert_eq!(score_session(&sess("web"), "this machine", "machine"), None);
        assert_eq!(score_session(&sess("web"), "this machine", "mac"), None);
        // No field matches → filtered out.
        assert_eq!(score_session(&sess("web"), "", "qqq"), None);
    }

    #[test]
    fn visible_sessions_ranks_while_filtering_and_groups_at_rest() {
        // Name-sorted resting list: apollo(boxB), banana(boxA), cherry(boxB).
        let mut apollo = sess_on("apollo", "boxB");
        apollo.cwd = "/w/one".into();
        let banana = sess_on("banana", "boxA");
        let mut cherry = sess_on("cherry", "boxB");
        cherry.headline = Some("apollo related".into());
        let mut a = App::test_fixture_hub(
            vec![apollo, banana, cherry],
            vec![machine("boxA", "hostA", true), machine("boxB", "hostB", true)],
            "",
        );
        // Resting: grouped by machine in /api/machines order (boxA first).
        assert_eq!(a.visible_sessions(), vec![1, 0, 2]);
        let rows = a.switcher_rows();
        assert_eq!(rows.len(), 5, "2 headers + 3 sessions");
        assert!(matches!(&rows[0], SwitcherRow::Header { label, .. } if label == "hostA"));
        assert!(matches!(rows[1], SwitcherRow::Session(1)));
        assert!(matches!(&rows[2], SwitcherRow::Header { label, .. } if label == "hostB"));
        // Filtering: flat, ranked — apollo (name) above cherry (headline);
        // banana drops out. Headers are suppressed.
        a.test_set_query("apollo");
        assert_eq!(a.visible_sessions(), vec![0, 2]);
        assert!(a.switcher_rows().iter().all(|r| matches!(r, SwitcherRow::Session(_))));
        // The cursor + actions resolve through the filtered view.
        assert_eq!(a.selected_session().unwrap().name, "apollo");
        a.move_sel(1);
        assert_eq!(a.selected_session().unwrap().name, "cherry");
    }

    #[test]
    fn switcher_rows_stay_flat_in_direct_and_single_machine_mode() {
        // Direct mode: no headers ever (byte-identical resting render).
        let a = App::test_fixture(vec![sess("alpha"), sess("beta")], "");
        assert!(a.switcher_rows().iter().all(|r| matches!(r, SwitcherRow::Session(_))));
        // Hub with ONE machine: also flat.
        let a = App::test_fixture_hub(
            vec![sess_on("alpha", "boxA")],
            vec![machine("boxA", "hostA", true)],
            "",
        );
        assert!(!a.multi_machine());
        assert!(a.switcher_rows().iter().all(|r| matches!(r, SwitcherRow::Session(_))));
    }

    #[test]
    fn key_list_types_into_query_and_letter_commands_are_dead() {
        let mut a = App::test_fixture(vec![sess("alpha"), sess("beta")], "");
        // Former command letters now type into the query and trigger nothing.
        for c in ['n', 'x', 'q', 'j', 'k', 'r', 'e'] {
            a.key_list(key(KeyCode::Char(c)));
        }
        assert_eq!(a.query(), "nxqjkre");
        assert!(matches!(a.overlay, Overlay::None), "no overlay opened by bare letters");
        assert!(!a.should_quit(), "q must not quit");
        // Backspace pops; Ctrl+U clears.
        a.key_list(key(KeyCode::Backspace));
        assert_eq!(a.query(), "nxqjkr");
        a.key_list(ctrl('u'));
        assert_eq!(a.query(), "");
    }

    #[test]
    fn key_list_esc_clears_query_then_quits() {
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.key_list(key(KeyCode::Char('a')));
        assert_eq!(a.query(), "a");
        a.key_list(key(KeyCode::Esc));
        assert_eq!(a.query(), "", "first Esc clears the query");
        assert!(!a.should_quit());
        a.key_list(key(KeyCode::Esc));
        assert!(a.should_quit(), "second Esc quits");
    }

    #[test]
    fn key_list_esc_with_fill_target_cancels_to_grid() {
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        a.fill_target = Some(0);
        a.mode = Mode::Switcher;
        a.key_list(key(KeyCode::Char('z')));
        a.key_list(key(KeyCode::Esc)); // clears the query, stays in the switcher
        assert!(matches!(a.mode, Mode::Switcher));
        a.key_list(key(KeyCode::Esc)); // cancels back to the grid
        assert!(matches!(a.mode, Mode::Grid));
        assert!(!a.should_quit());
    }

    #[test]
    fn key_list_ctrl_chords_act_with_an_active_query() {
        let mut a = App::test_fixture(vec![sess("alpha"), sess("beta")], "");
        a.test_set_query("alp");
        // Ctrl+X kills the selected (filtered top) row, not a hidden one.
        a.key_list(ctrl('x'));
        match &a.overlay {
            Overlay::Confirm { session, graceful } => {
                assert_eq!(session, "alpha");
                assert!(!graceful);
            }
            _ => panic!("Ctrl+X should open the kill confirm"),
        }
        a.overlay = Overlay::None;
        a.key_list(ctrl('e'));
        assert!(
            matches!(&a.overlay, Overlay::Confirm { graceful: true, .. }),
            "Ctrl+E opens the graceful-exit confirm"
        );
        a.overlay = Overlay::None;
        a.key_list(ctrl('r'));
        assert!(
            matches!(&a.overlay, Overlay::RenameSession(f) if f.session == "alpha"),
            "Ctrl+R renames the selected filtered row"
        );
    }

    // Regression (0059 Wave-2 verify): a slow async reply must dismiss only the
    // overlay it owns. Before the guard, an in-flight rename/create reply landing
    // after the user Esc'd and opened a *different* overlay would silently wipe it.
    fn rename_form(name: &str) -> RenameForm {
        RenameForm { session: name.into(), machine: String::new(), value: name.into(), error: None }
    }

    #[test]
    fn labeled_ok_dismisses_only_its_own_overlay() {
        let mut a = App::test_fixture(vec![sess("alpha")], "");
        // A foreign overlay (a delete-confirm) opened after the rename was Esc'd:
        // a stray SetLabel success must NOT close it.
        a.overlay = Overlay::Confirm { session: "alpha".into(), graceful: true };
        a.handle_labeled(Ok(()));
        assert!(matches!(a.overlay, Overlay::Confirm { .. }), "foreign overlay must survive");
        // …but it does close its own rename overlay.
        a.overlay = Overlay::RenameSession(rename_form("alpha"));
        a.handle_labeled(Ok(()));
        assert!(matches!(a.overlay, Overlay::None), "rename overlay closes on success");
    }

    #[test]
    fn created_ok_does_not_clobber_a_rename_overlay() {
        let mut a = App::test_fixture(vec![], "");
        a.overlay = Overlay::RenameSession(rename_form("alpha"));
        a.handle_created(Ok(("newname".into(), String::new())));
        assert!(
            matches!(a.overlay, Overlay::RenameSession(_)),
            "a rename overlay must survive a stray Created(Ok)"
        );
        // Its own new-session overlay still closes.
        a.overlay = Overlay::NewSession(NewForm::new(String::new(), 0));
        a.handle_created(Ok(("newname".into(), String::new())));
        assert!(matches!(a.overlay, Overlay::None), "the new-session overlay closes on success");
    }
}
