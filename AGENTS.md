# AGENTS.md

Guidance for AI agents (and humans) working in this repo.

## What this is

**cc-screen-rust** drives AI coding CLIs — `claude`, `codex`, `gemini`, `kimi` —
as long-lived terminal sessions you attach to from elsewhere. It's a **web-only,
tmux-free Rust rewrite** of cc-screen's Go `web/` daemon: each session owns a PTY
in-process (no tmux), the backend keeps an authoritative screen model + a
raw-output replay ring, and clients attach over a WebSocket.

Two clients speak the same wire contract:

- a **React PWA** (`frontend/`), embedded in the server binary — the phone/browser UI;
- **`ccs`** (`crates/tui/`), a native **terminal client** with a session switcher
  and a multi-pane grid.

It is **tailnet-only by design**: the agents launch with
`--dangerously-skip-permissions` / YOLO and the server must never bind a public
interface. "Remote" means another machine on your Tailscale network. Auth is
**opt-in** (off by default): set `CCWEB_PASSWORD` and/or `CCWEB_API_TOKEN` (see
`src/auth.rs`) to gate it — a thin guard against *other* tailnet users, not a
public-internet hardening.

## Workspace layout

The repo is a Cargo workspace; the root package (the server) doubles as the
workspace root so the `rust-embed` path and the build/install scripts don't move.

| Path | What |
|------|------|
| `src/` | **the server** (package `cc-screen-rust`): axum router, the session engine, HTTP+WS handlers, files/upload/clip, the embedded frontend |
| `crates/protocol/` | **`cc-screen-protocol`** — the shared HTTP+WS wire types (`SessionInfo`, `WsClientFrame`, `key_bytes`, the `\x1bc` snapshot/paste constants). **Single source of truth**; both server and TUI depend on it. |
| `crates/tui/` | **`ccs`** — the terminal client (ratatui + crossterm + `alacritty_terminal`) |
| `frontend/` | the React PWA; built to `frontend/dist/` and embedded into the server at compile time |
| `PLAN.md` | server design + decisions (the tmux→engine rewrite) |
| `TUI_PLAN.md` | the `ccs` terminal-client design + milestones (M0–M5) |
| `README.md` | quick build/run + deployment notes |

## Build / test / run

`cargo`/`rustc` may not be on `PATH`; source the env first (the scripts do this):

```sh
. "$HOME/.cargo/env"
```

- **Server (embeds the frontend):** `./build.sh build` builds the frontend →
  `frontend/dist` → the release binary. The server uses `rust-embed` with
  `#[folder = "frontend/dist"]`, so **`frontend/dist` must exist before compiling
  the server** (`./build.sh fe` builds just it). `dist/` is gitignored.
- **Tests:** `cargo test --workspace` — runs the protocol, server, and tui suites.
  Tests are colocated in each module's `#[cfg(test)]`. The server has a real-PTY
  engine test; the TUI has render-regression tests via ratatui's `TestBackend`
  plus pure-logic unit tests (input encoding, layout geometry, url derivation).
- **Run the server:** `./target/release/cc-screen-rust --addr 127.0.0.1:8839`
  (flags: `--addr`, `--no-restore`).
- **Run the TUI:** `cargo run -p cc-screen-tui -- --server http://HOST:8839`, or the
  installed `ccs`. **It needs a real interactive TTY** — it can't be driven through
  a captured/piped shell. Config: `~/.config/cc-screen-tui/config.toml`.

## Architecture

### The session engine (`src/engine.rs`)

Each `Session` owns its PTY master for its whole lifetime (not per-WebSocket) —
that's what lets input work with no client attached. A blocking reader thread
fans PTY output into three sinks: a `vt100` parser (preview line), a bounded
**raw-byte ring** (~768 KB, replayed on every (re)attach prefixed with `\x1bc`
RIS so a fresh emulator repaints), and a **broadcast channel** (live fan-out).
Restart model is **resume-only**: a redeploy ends the agents; auto-restore
relaunches each with its CLI's resume flag.

### The wire contract (`src/handlers.rs`, shared via `crates/protocol`)

REST for the session list + lifecycle (`/api/sessions`, `/api/session[/delete]`,
`/api/tools`, `/api/sessions/restorable|restore`, favorites, files/upload/clip);
one **WebSocket** per attached session (`/api/ws?session=`) carrying raw PTY
bytes out, and `{t:"i",d}` input / `{t:"r",c,r}` resize in (input may also be a
raw **binary** WS frame). **When you touch the contract, change
`crates/protocol`, not inline copies** — the server serializes these and the TUI
deserializes the mirror; drift breaks both clients and the React PWA.

### The TUI (`crates/tui/`)

`ccs` is a ratatui/crossterm app with one unified `mpsc<AppMsg>` event loop
(`app.rs`). Two modes: a **switcher** (session list + create/kill/restore
overlays) and a **grid**. Both list surfaces lead with the **`Recent` section**
([0078]) — the most-recently-focused sessions, persisted per hub host in
`~/.config/cc-screen-tui/state.toml` (never `config.toml`). Each attached box is a `Pane` (`pane.rs`) — an
`alacritty_terminal` emulator (chosen over `vt100` for real multi-thousand-line
scrollback) fed by the session WebSocket and rendered straight into the ratatui
buffer by a custom widget. The grid (`layout.rs`, `ui/grid.rs`) has the web app's
6 layouts, a visual layout palette, click/spatial focus, and a scoped
session-picker. Input is a tmux-style **`Ctrl-A` prefix**; `input.rs` encodes
crossterm `KeyEvent`s → VT byte sequences. Module map: `client/{rest,ws,url}`,
`ui/{switcher,grid,statusbar,overlay,util}`, `config.rs`, `term.rs` (RAII
terminal guard + panic hook).

## The hub (aggregator) — `crates/hub`

An optional **hub** lets one endpoint front many machines. Each machine runs the
**agent** (this server) which dials *out* to the hub over a single WebSocket
(`src/uplink.rs` → `/agent/ws`) and registers; clients (PWA + `ccs`) talk to the
**hub**, which transparently relays each request to the owning agent. The hub
**owns no PTY and no filesystem** — it's a registry + client-auth gate + byte
relay (`crates/hub/`: `registry`, `uplink_server`, `client_ws`, `watch_ws`,
`handlers`). The agent stays **dual-mode** (still serves direct clients) unless
`--hub-only`.

- **The load-bearing invariant:** every browser/`ccs` client maps 1:1 to a real
  `register_client()` subscriber on the owning agent, tunneled over a logical
  channel. The transport-agnostic `attach_loop` (`src/attach.rs`) is driven by
  both the local axum WS handler and the uplink, so the engine (`engine.rs`) is
  untouched and the snapshot-first / per-client-min-size / `Lagged`→resync
  invariants hold across the relay. **Don't break that 1:1 mapping.**
- **The envelope is the contract** (`crates/protocol/src/hub.rs`, feature `hub`):
  manual length-prefixed frames (`[u32 header_len][JSON header][raw payload]`);
  PTY bytes ride the raw tail, never base64/serde. `machine` is added to
  `SessionInfo` (`#[serde(default, skip_serializing_if)]` — omitted = single
  agent, so older clients still parse). Lifecycle/small-file ops route via
  `Cmd`/`Reply` (req-id correlated); terminal + fs-watch are per-`ch` channels.
- **Two independent credentials:** clients authenticate to the hub with the same
  `cc-screen-auth` gate (cookie/bearer); agents authenticate to the hub with a
  **separate per-agent uplink token** (`CCHUB_AGENT_TOKENS=machine:token,…`).
  A leaked client password can't impersonate an agent; a leaked agent token
  scopes to one machine.
- **Not yet relayed (documented gap):** bulk binary transfers — download with
  `Range`, 500 MiB upload, clipboard-image — over the dedicated `/agent/bulk`
  stream. Browse/edit (small file ops) + fs-watch + terminal + lifecycle ARE
  relayed. The PWA also still needs `machine` threaded through its components
  (the `ccs` TUI is fully threaded; `wsURL` already accepts `machine`).

## Conventions & gotchas

- **Tailnet-only, YOLO agents.** Never add a public bind *to an agent*. The TUI
  takes one base URL and derives `ws`/`wss` by scheme-swap.
- **Hub security model.** The rule isn't "never aggregate" — it's "the YOLO box
  never accepts inbound (it only dials out; `--hub-only` drops its local bind)
  and the relay never touches a filesystem." The hub concentrates access to every
  connected agent's PTYs/filesystem, so hub compromise = fleet blast radius:
  enable client auth in multi-machine mode, use per-agent uplink tokens, bind the
  hub's tailnet IP by default, and for off-tailnet use front it with a TLS
  reverse proxy (mTLS on the uplink). The agent's `confine.rs` ($HOME confinement)
  stays the authoritative guard — the hub can't widen it (file ops run on the agent).
  See **HUB.md → "Off-tailnet via a Cloudflare Tunnel"** for a concrete loopback-bind
  + tunnel recipe (and the 502 / open-uplink gotchas), and **HUB.md → "Running more
  than one agent on a single host"** for the isolated-`$HOME` + hand-written-unit
  pattern (the service name + `$HOME/.config/cc-screen-rust` state dir are fixed, so
  `install` is one-agent-per-host).
- **Auth is opt-in (`src/auth.rs`).** Off unless `CCWEB_PASSWORD`/`CCWEB_API_TOKEN`
  is set. The browser rides a signed 2-week session cookie (so individual
  fetches/WS need no token); headless clients (`ccs`, scripts) send
  `Authorization: Bearer <token>`. The middleware exempts static assets +
  `/api/{login,auth,logout}`; everything else under `/api/` is gated.
- **Per-session launch policy (0005, trimmed by 0014).** Each session has **one**
  create-time switch, `CreateReq.skip_permissions` (defaulted on so an older
  client reproduces today's behavior — YOLO). **Skip permissions** gates the
  tool's `yolo_flag` (split out of the launch template in `src/tools.rs`; declare
  a custom one with `cc_tool_yolo <cmd|prefix> <flag>`); it persists in the
  manifest for restore and both clients surface the toggle + a "safe" badge.
  **0014 removed the second switch** (`remote_control` / hub view-only): every
  hub session is now editable — there is no view-only gate, no agent-side `403`,
  no client badge, and "remote control" in the product refers *only* to Claude
  Code's own `claude --rc` desktop registration. See `cc-screen-saas` proposals
  0005 and 0014.
- **Default claude launch is plain `claude` (0015).** The built-in `cc` template
  in `src/tools.rs` no longer bakes in `--rc`/`--remote-control`, so a session no
  longer auto-registers with the Claude *desktop app* ("Remote control active") —
  cc-screen drives sessions over its own agent/hub/PTY path and resume uses
  `--continue`, not the registered name. Desktop registration is **opt-in** via a
  `tools.conf` override that keeps `{name}` substitution:
  `cc_tool cc claude "claude --rc 'claude-{name}'"`. Restore rebuilds from the
  current template, so the change applies uniformly with no migration. See
  proposal 0015. **Amended by 0082** (below): the default is no longer merely
  "no flag", and the opt-in is now a first-class per-session switch — the
  template-rewrite opt-in still works and bypasses the whole mechanism.
- **The assistant's own remote control is deterministic, per session (0082).**
  "Off by absence of a flag" wasn't off: Claude Code turns remote control
  (claude.ai/code + the mobile app) on from the user setting
  `remoteControlAtStartup`, from `/remote-control`, from **resume** — which is
  exactly what cc-screen's `--continue` and [0049]'s restart-and-resume run —
  and potentially from an upstream rollout. So **every launch of a
  remote-control-capable assistant now carries an explicit stance**, in
  `build_launch` beside the yolo gate: off (the default) appends
  `--settings <config_dir>/claude-remote-off.json` (content
  `{"disableRemoteControl": true}`, written at boot **and re-verified in
  `create`** so it can never dangle), on appends `--rc claude-{name}` — the
  first **flag-side `{name}` substitution** (templates were the only
  substituted thing before). Load-bearing details: the flag is a **file path,
  not inline JSON**, because a path rides the platform-split `shell_quote`
  under both `sh -c` and `cmd.exe /C` (0051's bug class); the **inline guard**
  blanks both flags when a template already spells `--rc`/`--remote-control`/
  `disableRemoteControl` out (but **not**
  `--remote-control-session-name-prefix`, which is naming-only — a false
  positive there silently leaves sessions capable); `remote_off_flag`/
  `remote_on_flag` are per-tool metadata, so codex/gemini/kimi/shell launch
  **byte-identically** to before, and `cc_tool_remote_off|on <tool> ""` is the
  stance-free escape hatch. The wire/manifest name is
  `assistantRemoteControl` — **never** reuse `remoteControl`, retired by 0014
  and pinned as never-serialized — it defaults **false** (so an old client
  creates a safe session with no shim), persists in the manifest, and
  `SessionInfo` reports the **effective** stance (requested ∧ the tool having
  an on-flag) so a badge can't claim a registration that didn't happen. See
  proposal 0082.
- **Assistant-CLI preflight & runtime guard (0046).** cc-screen *drives* external
  CLIs, so a missing binary is handled explicitly, all fed by one registry: the
  `Assistant` descriptors in `src/tools.rs` (name → label → per-OS install
  command; the probe binary is the first bare token of the launch template, so a
  `tools.conf` rename probes correctly; the `shell` tool is exempt). Enforcement:
  `cc-screen-rust doctor` (✓/✗ over the **session** PATH; `--install` offers each
  missing CLI's installer on a TTY, `--strict` for CI) is called best-effort by
  `install.sh`, `scripts/install-machine.sh`, and `cc-screen-rust install` — a
  missing assistant never aborts an install. At runtime `create_core` returns
  **424** + the install one-liner instead of spawning a doomed session (nothing
  registered, nothing recorded → no restore loop), `restore_all` skips (into
  `failed`, entry kept) a recorded session whose CLI vanished, and `GET
  /api/tools` sets the additive `unavailable` flag so both pickers grey the tool
  out. Custom tools declare a hint with `cc_tool_install <cmd|prefix> "<cmd>"`;
  adding an assistant = one `ASSISTANTS` entry + one `defaults()` line. See
  proposal 0046.
- **Update the assistants, then restart their sessions (0049).** The 0046
  registry gained an **update column** (`Assistant.self_update` /
  `update_macos|linux` / `version_arg`, overridable per machine with
  `cc_tool_update <cmd|prefix> "<cmd>"`), and `src/assistants.rs` runs it: ordered
  candidates (override → the CLI's own `update` → the package manager), each with
  a timeout and **no TTY**, and the verdict is a **before/after `--version`
  compare**, never an exit code (all four exit 0 with nothing to do). It's a
  **two-phase job**, not a request (`POST`/`GET /api/assistants/update` →
  `UpdateJob`): phase 1 `updating` the CLIs, phase 2 `restarting` the sessions
  that use them. Update-first is deliberate — a failed update leaves the machine
  untouched. The load-bearing bit is `Inner.restarting`: a clean `/exit` normally
  makes the reaper **forget** the manifest entry, so the restart marks the session
  and the reaper *consumes* the marker instead of forgetting — that's what makes a
  graceful stop safe (the CLI flushes the transcript `--continue` reads). Relaunch
  is `create(.., resume = true, ..)` + the colour/label re-apply `restore_all`
  does, under the **same name**, so panes re-attach by themselves. **No path here
  removes a manifest entry.** Over the hub the two `Cmd`s are gated on an additive
  `Register.caps` token (`assistant-update` → a pre-0049 agent gets a clean `501`,
  not a `504`) and on **ownership** — a 0039 share grants *use*, not
  administration. CLI parity is `doctor --update` (binaries only): `cc-screen-rust
  update` still means "update the agent itself". See proposal 0049.
- **Install the missing assistants (0050).** [0046] made *presence* a concern and
  [0049] made *staying current* one; 0050 makes **becoming present** an action.
  One runner (`src/provision.rs`) drives the registry's install column: non
  -interactive, **`$HOME`-only, never `sudo`**, with declared prerequisites
  (`Assistant.needs` → `PREREQS`: `npm` for codex/gemini, `uv` for kimi) installed
  the same way. The verdict is the **re-probe on the session PATH**, never the
  exit code — which is why the **landing zone** matters: an installer that drops
  the binary in a prefix the session PATH doesn't include gets a symlink (copy on
  Windows, no admin needed) into `~/.local/bin`, the one dir `build_env_path`
  guarantees, so `installed` means launchable *now* with no agent restart. Never
  clobbers a file it didn't put there. Three surfaces: the [0049] job gains a
  phase-1 branch (`installing`→`installed` — new **row states**, not a new
  `phase`, so an older client degrades legibly) plus a phase-2
  `restore_prefixes` that brings back the sessions the missing CLI was blocking;
  the dashboard row shows `N missing · Install`; and the machine-add one-liner
  takes a visible `--assistants` flag (`?assistants=` on Windows). CLI parity is
  `doctor --install --yes [--only a,b]` — the switch a piped `curl | sh` needs,
  since `--install` alone refuses without a TTY. Over the hub: owner-only (as
  [0049]), plus an additive `assistant-install` cap so an older agent gets a
  `501` instead of silently running an update-only job, `AgentMsg::Tools` to
  un-freeze the hub's register-time `unavailable` cache, and `UpdateBody` **must**
  stay `rename_all = "camelCase"`. See proposal 0050.
- **The Windows binary probe (0051).** `binary_on_path` split the PATH on `':'`
  and ignored `PATHEXT`, so on Windows it matched *nothing* — including
  `powershell`, the bare shell tool's head. Every create `424`d, every restore
  skipped, `/api/tools` greyed everything out and [0049]/[0050] were no-ops there.
  `tools::Resolver` now takes the separator + extension list as **inputs** (one
  implementation, unit-testable from Linux with a Windows-shaped PATH), follows
  `PATHEXT` rather than guessing (`.ps1` is *not* added when `PATHEXT` omits it —
  `cmd.exe /C` wouldn't run it either), and treats an absolute/drive/backslash
  head as a path. `resolve_on_path` returns *what* it found — `doctor` prints it
  and 0050's landing zone consumes it. Also: `--version` used to fall through to
  *serving*. See proposal 0051.
- **Clipboard image-paste shim (0007).** A Ctrl-V image paste from the web UI is
  staged in `src/clip.rs` (per-session, 20s TTL) and the paste key sent; Claude
  Code then shells out to `xclip`/`wl-paste`/`pbpaste` to *read* the image. The
  agent ships those as a **shim** (`scripts/clip-shim.sh`, embedded via
  `include_str!` and written to `~/.local/bin` by `cc-screen-rust install` /
  `install-shim`, idempotently — first on the session PATH). The shim resolves the
  image from, in order: (1) a **per-session local file** `$CCWEB_CLIP_FILE`
  (`$XDG_RUNTIME_DIR/cc-screen/clip/<session>.png`, 0600, freshness-gated by mtime)
  — the only path that works when the agent is **`--hub-only`** and binds no HTTP
  port; (2) **this agent over HTTP** (`$CCWEB_CLIP_URL` = the agent's *real* bind,
  NOT loopback, empty under `--hub-only`); (3) the legacy **Go** `cc-screen-web`;
  (4) the **Mac** clip-server (`:9999`). `clip_put` writes both the file and the
  in-memory slot on stage. Any non-image op (text paste, `-o`/`-i`, text
  `--list-types`) **defers to the real tool** (next PATH match via `type -aP`) —
  **amended by 0077 D1**, which takes ownership of the text *copy* branch
  (`pbcopy`/`wl-copy`) inside a cc-screen session.
  Why the file matters: clients reach sessions **through the hub**, the hub *does*
  relay `/api/clip` (bulk proxy → the agent stages it over the uplink), but a
  hub-only agent has no local HTTP for the shim to read back — the file closes
  that hop. Single source of truth is the one script — don't fork per-name copies.
  See `cc-screen-saas` proposal 0007.
- **Assistant-aware image delivery (0066).** The 0007 shim contract is now the
  `ClipboardProbe` arm of a per-tool `ImagePasteStrategy` (`src/tools.rs`,
  copied immutably onto `Session` at spawn — server-owned, never client
  input). **Codex** never runs the shims — it opens the X11/Wayland clipboard
  natively, absent on a headless box — so its strategy is
  `BracketedImagePath`: `clip_put` stages the PNG as a unique private file in
  the durable attachment store (`src/clip_attachment.rs`,
  `~/.config/cc-screen-rust/clip-attachments/<session>/`, 0700/0600, quotas
  64 files / 256 MiB per session / 1 GiB per agent → `507`) and
  bracketed-pastes its shell-escaped absolute path — **no Enter, no Ctrl-V**;
  Codex recognizes a pasted readable image path and attaches it. Attachments
  are *durable*: they survive restart/resume (a draft/transcript may still
  reference the path) and are removed only on permanent delete / clean
  non-restart exit / startup GC of unclaimed dirs. Status contract: `422` bad
  PNG, `507` quota, `503` PTY write failed (rollback only on a proven
  zero-byte write). No hub/wire change — the dispatch is agent-local. See
  `cc-screen-saas` proposal 0066.
- **Text clipboard: copy travels OUT in-band, and only to the driver (0077).**
  The image direction above is *inbound*; this is the opposite one, and it is
  the product's first **outbound** capability — session output can now act on
  the viewer's device. Four rules, all load-bearing:
  **(1) In-band, no new route.** A copy performed inside a session is the OSC 52
  the assistant already emits (`ESC]52;c;<b64>BEL`); the agent
  (`src/engine.rs` → `src/attach.rs`) and the hub relay terminal output
  verbatim, so the bytes already arrive — nothing in `crates/protocol` changed.
  A pre-attach sequence is unrecoverable by construction: `snapshot()`
  re-serializes grid state, not the byte stream.
  **(2) The read form is never answered.** `frontend/src/osc52.ts` imports
  nothing and has no send path — the refusal is *structural*, not a `?` check
  that `;p;?` or a base64-encoded `?` could slip past — and `crates/tui`
  sets `Osc52::OnlyCopy` explicitly. A vitest reads the module's source to pin
  it. **Never give that module access to a socket, a `Terminal`, or `fetch`.**
  **(3) Delivered to the DRIVING client only, never to every attached viewer.**
  One PTY fans out to every attached human ([0063] team sessions are
  read-only-*visible* by default and there is no per-connection observer flag —
  [0014] removed it), and the assistant runs `--dangerously-skip-permissions`,
  so a prompt injection yields arbitrary OSC 52. The silent tier therefore needs
  the active pane + DOM focus + a focused document + recent input from this
  client (`frontend/src/osc52Bus.ts`); everything else — a watcher, a multi-line
  payload, anything sanitisation altered — becomes a frozen click-to-copy toast.
  Plus: sanitisation (bare CR / trailing newline / C0-C1 / bidi), a 64 KiB cap,
  a per-session rate limit that disables after a flood, quiet periods after
  attach and after the user's own copy, and one acting surface per session (the
  grid pane and the editor's `AgentMirror` hold two sockets onto the same
  session — without the arbiter one copy is delivered twice).
  **(4) The agent never writes its OWN clipboard.** `SHIM_NAMES` gained
  `pbcopy`/`wl-copy` (`src/service.rs`); the shim's copy branch base64s stdin to
  `/dev/tty` as OSC 52, **guarded on `$CCWEB_SESSION`** so the machine owner's
  own `pbcopy` outside a session is untouched. Before this, a macOS agent's
  `pbcopy` succeeded and left the text on a shared machine's pasteboard while
  the assistant reported success.
  `ccs` re-emits on its own stdout for the host emulator to act on — it does
  **not** link a clipboard crate (a headless SSH box has no display server; the
  outer terminal does), and drops `Selection`-typed writes so a `p` store is
  never promoted. See `cc-screen-saas` proposal 0077.
- **The touch/wheel scroll ladder is one ladder, in both clients (0031/0069).**
  `flush()` in `TerminalView.tsx` is a three-rung precedence ladder copied from
  [0069]'s TUI implementation, not re-decided: normal buffer (or a locally
  scrolled-back viewport) → `scrollLines()`; alternate screen + mouse reporting
  → SGR wheel reports; alternate screen without → arrow keys, encoded per
  `applicationCursorKeysMode`. Rung 1 **fails closed** — anything unrecognised
  moves pixels, never bytes — and rungs 2/3 are clamped to 3 steps per flush
  with the surplus *discarded*, because every step is input a real program must
  parse. It rides `term.input()` → the existing `{t:"i"}`, so no protocol
  change. The gesture half is as load-bearing as the sink: `touch-action: none`
  on `.cc-term-host` plus capture-phase `stopPropagation` from `touchstart`,
  and **no `preventDefault` until the 8px deadband classifies a drag** — a tap's
  compatibility mouse events are what make tap-to-click work inside Claude
  Code's TUI. Which renderer a session uses is a per-install remote rollout
  gate, not a version or an OS, so both states must be tested. See proposal
  0031 (Strategies A + C).
- **An idle web client must draw nothing (0068).** The tab is the product's
  primary surface and stays open for days, so *continuous* work in the frontend
  is a bug: **no infinite CSS animation** outside a loading state (no cursor
  blink — `cursorBlink: false` in both terminal surfaces — no pulsing dots, no
  scanline), and closed overlays (`SessionDrawer`, `StatusView`) keep their
  root + transition but unmount their rows (`useOpenOrClosing`). Terminals
  render through **`@xterm/addon-webgl`, exact-pinned `0.18.0`** (0.19 targets
  xterm 6 and declares no peer dep — it would install silently broken beside
  5.5) via `src/xtermRenderer.ts`, which falls back to the DOM renderer on
  no-WebGL2 *and* on context loss and reports the live one as
  `window.__ccRenderer`. Because WebGL draws pixels, browser find-in-page can't
  see terminal output: **`⌃B /`** (header magnifier on touch, the agent-column
  magnifier in the editor) opens the in-pane find bar — `⌃B t` still reaches
  [0038]'s tree filter, and `Cmd/Ctrl+F` is still never hijacked. Every
  recurring poll goes through **`usePoll` (`src/poll.ts`)**: visible cadences
  are unchanged, hidden tabs pause (sessions keep a 60s heartbeat for the tab
  title + app badge) and refetch once on return. Poll payloads are applied only
  when they actually changed (`sameJson` / `sessionsKeyRef`) so the memoized
  `TerminalView`/`TileGrid`/`SessionDrawer`/`StatusView` don't re-render on a
  no-op tick — **keep the props those four receive stable** (`useCallback` /
  `useMemo`), or the memo silently stops working. See proposal 0068.
- **The selector answers "where was I" first, "what needs me" second (0078).**
  Both clients lead their resting session list with a **`Recent` section**: the
  most-recently-*focused* sessions, in stored order, lifted out of the machine
  groups. It is deliberately **not** attention-ordered — a section that
  re-sorts when an agent finishes is one you cannot build muscle memory on — so
  the only thing that moves a row is the user focusing a session, and never
  while the selector is open (membership is snapshotted at open). Below it the
  groups keep exactly today's order: web triage (ready-first / freshest state
  anchor), TUI name sort. **A typed query short-circuits the split entirely**,
  so [0028]'s ranking *and* its equal-score tie order (a stable sort over the
  resting list) stay bit-for-bit what they were. Identity is `(machine, name)`
  — `frontend/src/sessionRecents.ts` reuses `sessionKey`, `ccs` stores
  `{machine, name}` pairs — so a rename ([0035]) never reorders or duplicates
  an entry. Storage is **per client install**: `ccweb.sessionRecents.v1` in
  localStorage, and `~/.config/cc-screen-tui/state.toml` keyed per hub host,
  which is where `Config::recents` moved (it was dead, unkeyed, and rewrote the
  user-editable `config.toml` on every attach — the clobber [0060] Part A
  fixed). Three traps, all silent when broken: the **header run-detectors** in
  both clients (`SessionDrawer.tsx`'s `lastMachine`, `switcher_rows`/`menu_rows`
  in `app.rs`) must never see a section row, or the first machine header
  vanishes; **cursor/region arithmetic** that assumed "actions, then sessions"
  is now a lookup (`menu_initial` counts *selectable* rows, the drawer searches
  `baseItems`); and the e2e binary shares one `XDG_CONFIG_HOME`, so each booted
  app takes its own `set_state_scope()` or one test's attach reorders another's
  list. Absence from a poll never forgets an entry — only an explicit
  kill/delete, or the 20-cap, does. **Addendum:** the cursor parks on the top
  `Recent` row, so `⌃B`/`⌃A d` → `⏎` is "back to the last session" on desktop,
  phone, and `ccs` alike. See proposal 0078.
- **The web `⌃B` keymap, and the three rules it cost us to learn (0081).** The
  desktop prefix engine lives in `frontend/src/App.tsx` (arm/repeat state
  machine, capture-phase, `isDesktop`-gated) and its chord ladder is now written
  down for users in `site/docs/web-app.md#keyboard-shortcuts` — keep the two in
  sync, and add `⌃B ;` (last-pane) to any inventory you copy. Three rules bind
  anything you add there:
  **(1) Count panes, never layout ids.** `Layout` is `1..6` but `paneCount` is
  `{1,2,3,4,2,3}` — `⌃B ←/→` wrapped modulo the *id* and was a dead key in the
  stacked and right-tall layouts for as long as they existed. Every index
  arithmetic goes through `paneCount(layout)`.
  **(2) An uppercase chord must precede its lowercase twin, or discriminate on
  `e.shiftKey` inside one case.** The ladder is first-match-wins, so
  `if (k === "s" || k === "S")` shadowed a later `if (k === "S")` and [0041]'s
  `⌃B ⇧S` share chord never once fired.
  **(3) A focus signal is broadcast and filtered by the receiver — never gated
  by swapping the prop per pane.** `searchSeq` (`TileGrid` → `TerminalView`) is
  the reference implementation: hand every pane the same counter and test
  `active` in the effect, *consuming the bump before the active test* so an
  inactive pane can't replay it later. `renameSeq` did it the other way
  (`idx === active ? seq : -1`), which made *changing the focused pane* look
  like a bump — so every `⌃B ←/→` opened the rename box, the focused `<input>`
  then swallowed the whole prefix, and blur-commit POSTed a display label nobody
  asked for.
  Also: `shouldSkipShortcut` (`frontend/src/util.ts`) is the single gate keeping
  the prefix out of text fields, and it is shared with the mobile handler —
  widen it only by attribute, as the [0026] empty-pane filter (`data-pane-filter`)
  and xterm's helper textarea are. Panes carry `data-pane` / `data-pane-active`
  so the harness can ask which one is focused; `frontend/tools/smoke.mjs`'s
  `gridKeyboardPass()` is the coverage, and its readiness helper is
  `waitAttached` (the old `[title="open"]` selector matched nothing for months).
  See proposal 0081.
- **`crates/protocol` is the contract.** Keep JSON field names matching what the
  React PWA expects; the parity is covered by tests in the protocol crate.
- **Frontend must be built before the server compiles** (embedded at build time).
- **Keep tests green and the build warning-free** (`cargo build --workspace`);
  add a `#[cfg(test)]` test next to new logic.
- The TUI's terminal guard restores raw-mode/alt-screen/mouse on panic — don't
  bypass it.

## Deployment

Runs as the `cc-screen-rust` **systemd `--user`** service on **port 8839**, bound
to the host's Tailscale IP, **side-by-side** with the Go `cc-screen-web` (port
8838) — own config dir (`~/.config/cc-screen-rust/`), own session store, reusing
the `tools.conf` format. `./install.sh` builds + installs the unit. The `ccs`
binary is typically installed to `~/.local/bin/`.

To turn on auth, `cc-screen-rust install --password PW` writes `CCWEB_PASSWORD`
to `web.env` and auto-generates a `CCWEB_API_TOKEN` (printed once, for the TUI);
both are editable in `~/.config/cc-screen-rust/web.env`. Point the TUI at it via
`api_token` in `~/.config/cc-screen-tui/config.toml`, `ccs --token`, or
`CCS_API_TOKEN`/`CCWEB_API_TOKEN`. **Don't run `install`/`uninstall` to test** —
`systemctl --user` hits the live service.

The **hub** is its own binary + service: `cc-screen-hub install [--port N]
[--password PW] [--token TOK] [--agents machine:token,…]` (systemd `--user`
`cc-screen-hub.service`, default **port 8840**, config dir
`~/.config/cc-screen-hub/`). Agents opt in with `cc-screen-rust ... --hub
http://HUB:8840 --token <uplink-token> --machine-id NAME` (env:
`CCWEB_HUB_URL`/`CCWEB_HUB_TOKEN`/`CCWEB_MACHINE_ID`); `--hub-only` suppresses the
local bind. Same **don't run `install`/`uninstall` to test** rule applies to the
hub. Local two-process smoke: run both binaries on `127.0.0.1:18840`/`:18839`
under a temp `$HOME` (see the `examples/hub_attach_smoke.rs` client).

## Further reading

- **`PLAN.md`** — server design, decisions, parity notes.
- **`TUI_PLAN.md`** — the `ccs` design and milestones (M0–M5), including the
  emulator choice and the grid.
- **`HUB.md`** — the aggregator: setup for the hub + slaves + TUI, env-var
  reference, security model, what's relayed, and troubleshooting.

<!-- >>> dibbla skill >>> -->
## Dibbla CLI

This project uses the Dibbla CLI. Detailed guidance for agents using it lives at:

- `.claude/skills/dibbla/SKILL.md` — entry point (commands, flags, agent guidelines)
- `.claude/skills/dibbla/reference.md` — full command reference
- `.claude/skills/dibbla/examples.md` — example flows
- `.claude/skills/dibbla/guardrails.md` — safety checks

Installed by `dibbla skills install dibbla` (CLI 1.2.39). Re-run to refresh.
<!-- <<< dibbla skill <<< -->
