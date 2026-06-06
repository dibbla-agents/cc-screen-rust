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
overlays) and a **grid**. Each attached box is a `Pane` (`pane.rs`) — an
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
- **Per-session launch policy (0005).** Each session has two switches, chosen at
  create (`CreateReq.skip_permissions` / `.remote_control`, defaulted so an older
  client reproduces today's behavior — YOLO on, hub control off). **Skip
  permissions** gates the tool's `yolo_flag` (split out of the launch template in
  `src/tools.rs`; declare a custom one with `cc_tool_yolo <cmd|prefix> <flag>`).
  **Remote control** is the per-session *view-only-through-the-hub* gate, enforced
  authoritatively on the agent (`src/uplink.rs` drops input, `src/ops.rs` 403s
  key/paste/clear/delete) — the direct port is unaffected. Both persist in the
  manifest for restore; both clients surface the toggles + a "view only" badge.
  See `cc-screen-saas` proposal 0005 and HUB.md → "Per-session view-only control".
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
