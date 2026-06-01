# cc-screen-tui — plan

A **terminal client** for the cc-screen-rust backend. A native TUI that lists
sessions and attaches to one at a time, rendering the session's output into a
ratatui pane with a **persistent bottom status bar** so you always know which
session you're in. v1 is single-pane; the grid (2×2 + splits) is an additive
fast-follow.

## Decisions (locked)

- **Render model: embedded pane, not passthrough.** The client runs its own
  terminal emulator per attached session (fed by the same raw PTY byte stream
  the web's xterm.js consumes) and draws it into a ratatui `Rect`. This is what
  makes a persistent status bar possible — ratatui owns the whole screen, the
  agent only owns a sub-rectangle, so the bar survives the agent's alt-screen
  use. The *same* capability is what later enables the grid. (Passthrough was
  rejected: it gives perfect fidelity but the agent owns the whole terminal, so
  no bar and no grid.)
- **Emulator: `alacritty_terminal`** (rendered straight into a ratatui buffer by
  a small custom widget). Started on `vt100`+`tui-term` (matched the server's
  emulator), but swapped to alacritty for **real multi-thousand-line
  scrollback** — vt100 0.15's view is capped at one screen height (its
  `visible_rows` underflows past that). alacritty has a proper scrollback grid
  (`Config::default()` = 10000 lines, `scroll_display` for the wheel) and richer
  cell fidelity. The byte→screen path is one type (`Pane`), so the renderer +
  emulator stay localized.
- **Layout: Cargo workspace, shared `protocol` crate.** One source of truth for
  the wire contract so client and server can't drift.
- **Transport: existing wire contract, zero backend changes.** REST for the
  session list + lifecycle, one WebSocket per attached session for the byte
  stream + input/resize.
- **No auth.** The backend is tailnet-only by design; "remote" means another
  tailnet host. The client takes one base URL and derives `ws`/`wss` from it.
- **v1 scope cut vs the web app:** no file editor / download / PDF / upload /
  clipboard-image / multi-pane. Pure terminal + session management.

## Workspace restructure

Keep the server package **in place** (it doubles as the workspace root) so the
`rust-embed` path, `build.sh`, `install.sh`, and the systemd unit don't move.
Add two member crates:

```
cc-screen-rust/
├─ Cargo.toml          # [package] cc-screen-rust (server) + [workspace] members
├─ Cargo.lock          # shared
├─ src/                # server — UNCHANGED location
├─ crates/
│  ├─ protocol/        # cc-screen-protocol  (lib: shared wire types)
│  └─ tui/             # cc-screen-tui        (bin: `ccs`)
├─ frontend/           # unchanged
├─ PLAN.md  README.md  build.sh  install.sh
└─ TUI_PLAN.md
```

Notes:
- A root `Cargo.toml` can hold both `[package]` and `[workspace]`; the root
  package is implicitly a member, `target/` stays at the root (so
  `./target/release/cc-screen-rust` and `./target/release/ccs` both resolve and
  `build.sh`/`install.sh` keep working). `[profile.release]` stays in the root
  manifest and applies workspace-wide.
- Moving the server into `crates/server/` is possible but buys nothing here and
  would force fixing the `#[folder = "frontend/dist"]` embed path, the build
  scripts, and the profile location — so we don't.

### `crates/protocol` — what moves out of the server

Pure, dependency-light (`serde`, `serde_json`). The server imports these and
deletes its inline copies; behavior is unchanged (a mechanical refactor).

- `Session` — the `/api/sessions` DTO (`name, tool, short, attached, activity,
  preview, cwd`; `cwd` skipped when empty). Today's `handlers::SessionDto`.
- `Tool`, `RestorableSession`, `CreateReq`/response, `DeleteReq`, `Favorite` —
  the request/response shapes.
- `WsClientFrame` — the `{t,d,c,r}` frame, with constructors `input(&str)` and
  `resize(cols,rows)` and a `Deserialize` mirror of the server's `handle_frame`
  `M` struct. One definition, both sides.
- `key_bytes(name) -> Option<&'static [u8]>` — moved verbatim from `engine.rs`
  (it's pure); its unit test moves too. The server's `/api/key` and the TUI both
  use it.
- Constants: `SNAPSHOT_RESET = b"\x1bc"` (RIS), `PASTE_START = b"\x1b[200~"`,
  `PASTE_END = b"\x1b[201~"`.

Server churn is limited to: add the `cc-screen-protocol` dependency, replace the
inline `SessionDto`/`M`/`Favorite`/`key_bytes`/paste-byte literals with
`protocol::*`. No handler logic changes.

## `crates/tui` — architecture

Binary name **`ccs`**. Async on tokio; ratatui draws on the main task only.

```
crates/tui/src/
├─ main.rs        # clap parse → config → run(app)
├─ cli.rs         # args: --server <url>, [attach <session>], --insecure
├─ config.rs      # ~/.config/cc-screen-tui/config.toml (directories + toml)
├─ client/
│  ├─ url.rs      # base URL → REST base + ws/wss URL (scheme swap)
│  ├─ rest.rs     # reqwest(rustls): sessions, tools, create, delete,
│  │              #   restorable, restore, favorites, session_root
│  └─ ws.rs       # tokio-tungstenite(rustls): per-session attach,
│                 #   in: Bytes stream, out: WsClientFrame; backoff reconnect
├─ app.rs         # mode state machine (Switcher | Attached) + main select! loop
├─ event.rs       # merge: crossterm EventStream, WS bytes, poll tick
├─ pane.rs        # Pane: vt100 parser + WS task wiring + resize + conn state
├─ input.rs       # crossterm KeyEvent → VT bytes; bracketed paste; prefix key
├─ term.rs        # RAII terminal guard (raw mode / alt-screen / paste / panic hook)
└─ ui/
   ├─ switcher.rs   # session list: render + keys (enter/new/kill/exit/restore)
   ├─ attached.rs   # the single pane (tui-term widget) + bottom bar layout
   ├─ statusbar.rs  # the bar widget
   └─ newsession.rs # create-session modal (tool + name + dir)
```

### Async design

- One `tokio::select!` loop in `app.rs` over:
  - `crossterm::event::EventStream` (feature `event-stream`) — keys, **resize**,
    bracketed-`Paste` events.
  - a merged WS-bytes channel `mpsc<(PaneId, Bytes)>` — the attached pane's task
    ships raw bytes here.
  - a poll `interval` (~1 s) — refresh the session list / bar liveness.
- The **Pane owns the `vt100::Parser`** in main-loop state; the pane's network
  task does I/O only and forwards raw `Bytes`. So all parsing + rendering is
  single-threaded (ratatui's requirement) and there's no lock on the screen.
- The pane task owns the split `WebSocketStream`: it receives outgoing
  `WsClientFrame`s on an `mpsc` (from the main loop) and pushes incoming binary
  frames to the bytes channel; it reconnects with 500 ms→5 s backoff (mirrors
  `TerminalView`).
- **Coalesce before draw:** drain all pending WS bytes, `parser.process(...)`
  them, then draw once. Avoids redraw thrash under heavy output (ratatui still
  diffs, but this caps it to one frame per event burst).

### The pane (`pane.rs`)

- Holds `vt100::Parser::new(rows, cols, SCROLLBACK)`, a `PaneId`, the outgoing
  `mpsc<WsClientFrame>` sender, and a `ConnState` (`Connecting|Open|Closed`).
- On attach: open WS, the server's first frame is the `\x1bc`-prefixed snapshot
  (history replay) → fed straight into the parser; the live stream patches it.
  **Verify `vt100` honors `ESC c` (RIS)**; if it doesn't fully reset, recreate
  the `Parser` whenever a frame *starts* with `SNAPSHOT_RESET` (reconnect /
  lagged-resync both arrive RIS-prefixed).
- Render: `tui_term::widget::PseudoTerminal::new(parser.screen())` into the
  pane `Rect` (the area minus the 1-row bar).
- Resize: on `Event::Resize` or layout change, recompute inner cols×rows,
  `parser.set_size(rows, cols)`, send `WsClientFrame::resize(cols, rows)`.
  Debounce ~50 ms (drag-resize spams). Send an initial resize right after the WS
  opens (matches `TerminalView`'s `onopen`).

### Input (`input.rs`) — the real new cost

Encode crossterm `KeyEvent` → VT byte sequence for the focused pane:

- Printable char → its UTF-8 bytes. `Enter`→`\r`, `Tab`→`\t`,
  `BackTab`→`\x1b[Z`, `Backspace`→`\x7f`, `Esc`→`\x1b`.
- Arrows / `Home` / `End`: honor the parser's **application-cursor mode**
  (`screen().application_cursor()`) → `\x1bO{A..D}` / `\x1bOH` / `\x1bOF`,
  else `\x1b[{A..D}` / `\x1b[H` / `\x1b[F`.
- `PageUp/Down`→`\x1b[5~`/`\x1b[6~`, `Insert/Delete`→`\x1b[2~`/`\x1b[3~`,
  `F1..F12` → standard sequences.
- `Ctrl`+letter → the control byte (`Ctrl-A`=`0x01` … `Ctrl-Z`=`0x1a`).
  `Alt`+key → `ESC` prefix + the key's bytes.
- Bracketed paste: enable it; on `Event::Paste(s)` send
  `PASTE_START + s + PASTE_END` (matches the server's `/api/paste`). This is the
  clean multi-line-paste path.
- Pure functions → unit-testable exhaustively (the regression net for "key X
  does nothing").

### Prefix key (tmux-style, default `Ctrl-A`, configurable)

The main loop intercepts before forwarding to the pane:
- not in prefix state + key == prefix → enter prefix state.
- in prefix state + key == prefix → send a **literal** prefix to the pane
  (escape hatch).
- in prefix state + command: `d` detach → Switcher, `x` kill (confirm), `c`
  create, `s` open switcher overlay, `?` help. (Digit-switch / `[`/`]` reserved
  for the grid.)
- otherwise encode + send.

### Status bar (`statusbar.rs`)

Bottom 1 row, drawn by ratatui *outside* the pane rect — so it's rock-solid even
when the agent is on the alt-screen (the whole reason for the embedded model).
Shows: `[claude-myproj]`, tool prefix, `●`live/`○`idle, pane size `80×23`,
connection state (`connecting…`/`open`/`reconnecting`), right-aligned prefix
hint `^A ?`.

### Terminal guard (`term.rs`)

RAII + a panic hook that restores the terminal on *any* exit path: leave
alt-screen, disable raw mode, disable bracketed paste, show cursor. Without this
a panic leaves the user's shell wrecked. Install before entering the UI.

### Connecting to a server

- One base URL: `--server` flag → else config `server` → else
  `http://127.0.0.1:8839`. `url.rs` derives REST base + WS URL by scheme swap
  (`http→ws`, `https→wss`) exactly like the web `wsURL()`.
- rustls everywhere (no OpenSSL system dep). Valid certs (e.g. `tailscale
  serve`) work out of the box via `webpki-roots`; `--insecure` accepts invalid
  certs for ad-hoc self-signed setups.
- Config `~/.config/cc-screen-tui/config.toml`: `server`, `prefix` (e.g.
  `"C-a"`), `recents = [...]`, optional `[servers]` map (no UI in v1; the file is
  the source).
- REST/WS failures surface as a transient bar message, never a crash; the
  switcher shows "server unreachable — retrying" and keeps polling.

## Status — v1 complete (M0–M4), all milestones tested ✅

Binary `target/release/ccs`. 16 tui unit/render tests + the protocol/server
suites all pass; every milestone also has a live PTY smoke test. Run it with
`ccs --server http://127.0.0.1:8839`.

## Milestones (v1)

- **M0 — Workspace + protocol crate.** ✅ Shared types extracted to
  `crates/protocol`; server refactored to use them with byte-identical JSON; its
  tests still pass.
- **M1 — TUI skeleton + switcher.** ✅ clap, config, REST client, a polling
  switcher; TestBackend render regression + live PTY smoke.
- **M2 — Single-pane attach (read-only) + bar.** ✅ WS → `vt100` → `tui-term`
  pane + persistent bottom bar. **Fidelity-gate live test passed** (snapshot +
  live stream + bar). The `Pane` emulator sits behind one type, so a future swap
  to `alacritty_terminal` is localized. *Remaining manual check: eyeball a real
  Claude/Codex session.*
- **M3 — Input.** ✅ KeyEvent→VT encoding (DECCKM-aware cursor keys, function
  keys, Ctrl/Alt, bracketed paste), input as binary WS frames, resize as JSON,
  tmux-style prefix key (`Ctrl-A` d=detach, x=kill, prefix-prefix=literal).
  Exhaustive encoder unit tests + live typing/detach smoke.
- **M4 — Lifecycle + polish.** ✅ New-session form (tool/name/dir), kill/exit
  confirm, restore-all; WS reconnect with backoff; auto-detach when the attached
  session ends; recents persisted to config. Live create/kill/auto-detach smoke.

### Deferred (out of v1)
- **`--insecure` for `wss`**: honored on the HTTP side (reqwest); the WebSocket
  side does `ws`/valid-`wss` only.
- A restorable *picker* (restore-all is wired; the per-session list isn't).
- `ccs attach <session>` direct-attach flag (the switcher covers it).

## M5 — Multi-pane grid (next) — design locked

A port of the web app's `TileGrid` + `LayoutPalette` to the terminal, so the
mental model + shortcuts match across web and TUI.

- **Layouts: the web app's 6 presets** (same digit order). `paneCount`:
  `{single:1, stacked:2, side-by-side:2, left-L:3, right-L:3, quad:4}`.
  `tiles(layout, area) -> Vec<Rect>` replaces the CSS-grid templates (ratatui
  `Layout` splits; pane index → rect must match the web's `pane(i)` mapping so
  slot 0 is always the primary).
- **Visual picker**: a centered overlay (same machinery as the confirm /
  new-session modals) showing the six box-glyph thumbnails, current
  highlighted; `←/→` move, `1-6` jump, Enter apply, Esc cancel. Opened with
  `Ctrl-A l` (mirrors web's `Ctrl+B l`). On apply: resize `panes` to the new
  count, **migrate the focused session into slot 0** (web parity), spawn/abort
  WS tasks as boxes appear/disappear.
- **Each box** is a bordered `Block` with its session name in the top edge; the
  **focused box's border is accent-colored** (the "which session am I in" cue,
  now per-box). Empty boxes show `⏎ to pick`.
- **Focus + fill**: `Ctrl-A` + arrows / a digit / **mouse click** focuses a box;
  keystrokes and the wheel route to the focused box. Enter on an empty/focused
  box opens the switcher *scoped to that box* (the chosen fill flow). **Dedupe**
  — a session may occupy at most one box (they'd fight over the one PTY width).
- **State**: `App` goes from `pane: Option<Pane>` to `layout: Layout`,
  `panes: Vec<Option<Pane>>`, `active: usize`. Per-pane resize on layout /
  terminal change.

Mostly additive — `Pane` (alacritty + WS + render), the overlay system, the
switcher, mouse capture, and per-pane resize already exist; M5 is one→N panes
plus `tiles()`, the palette overlay, and focus routing.

### Sub-steps
- **M5a** ✅ — `layout.rs` (6 presets, `tiles()`/`inner_rects()`, 9 geometry
  tests). App: `panes: Vec<Option<Pane>>` + `layout`/`active`/`fill_target` +
  per-pane `id` routing. Bordered grid render (focus accent, empty `⏎ to pick`,
  `Single` stays borderless) + render test. Per-pane resize (`relayout`).
  Dedupe. Interim controls: `Ctrl-A` digit = layout, `Ctrl-A` arrows = focus,
  Enter on an empty box → switcher fill. Live grid test passes.
- **M5b** ✅ — the visual layout-palette overlay (6 box-glyph thumbnails,
  current highlighted; `←/→` move, `1-6` jump-apply, Enter apply, Esc cancel),
  opened with `Ctrl-A l` / `Ctrl-A Space` over the grid. `set_layout` migrates
  the focused session into slot 0 (web parity). `Ctrl-A <digit>` kept as a
  direct power-shortcut. Palette render test + live test (open / digit-apply /
  migrate).
- **M5c** ✅ — focus polish: mouse **click-to-focus** a box (click an empty box
  → picker), **wheel scrolls the box under the cursor**, **spatial arrow nav**
  (`Ctrl-A` arrows pick the nearest box by geometry), and a **scoped
  session-picker overlay** over the grid (replaces the full-screen switcher
  fill; `n` there creates into the box). Picker render tests + live test.

**M5 complete** — the multi-pane grid (6 layouts, visual palette, click/spatial
focus, scoped fill) is done. 32 tui tests + the protocol/server suites pass.

### Beyond v1 (not planned)
Arbitrary BSP (recursive splits, drag-resize, 5+ panes) — the 6 presets cover
the practical ≤4-box arrangements; revisit only if they prove limiting.

## Testing

- `protocol`: frame (de)serialization round-trips against the server's expected
  JSON; the `key_bytes` map test (moved from `engine.rs`).
- `input.rs`: exhaustive KeyEvent→bytes table tests (incl. app-cursor mode).
- `url.rs`: base→ws/wss derivation.
- `pane.rs`: feed a known byte script into the parser, assert screen contents
  (the analog of the server's real-PTY engine test).
- Render regression: build a `Pane`, process bytes, render with ratatui's
  `TestBackend`, assert the buffer (catches bar-layout / pane-rect breakage).
- Manual: run against a local `shell`-tool session for deterministic output;
  judge Claude/Codex fidelity by eye (the one thing unit tests can't settle).

## Risks & how each is handled

- **Fidelity (the big one).** Gated at M2 before any further investment;
  emulator behind a `Pane` trait so `alacritty_terminal` is a localized swap.
- **RIS on (re)attach.** Verify `vt100` resets on `ESC c`; fallback is to
  recreate the parser on a `SNAPSHOT_RESET`-prefixed frame.
- **Input completeness.** App-cursor mode + function keys are the usual gaps;
  the pure encoder + tests make iteration safe.
- **Multi-client resize tension (pre-existing).** The backend has one PTY per
  session at one size — last resizer wins. Different sessions in a grid are
  fine; it only bites when the *same* session is open at two sizes (TUI pane vs
  phone). Inherent; `clear-history` is the escape hatch. Documented, not solved.
- **Terminal wreckage on panic.** The RAII guard + panic hook in `term.rs`.

## Deferred (explicitly out of v1)

Mouse forwarding into the pane, in-pane scrollback / copy mode, the grid (M5),
file/editor/upload/clipboard/PDF, a named-server picker UI, and any SSE/WS push
for the session list (polling is fine).
```
