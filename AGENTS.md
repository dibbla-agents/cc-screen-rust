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
`--dangerously-skip-permissions` / YOLO, there is **no auth**, and the server must
never bind a public interface. "Remote" means another machine on your Tailscale
network.

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

## Conventions & gotchas

- **Tailnet-only, no auth, YOLO agents.** Never add a public bind. The TUI takes
  one base URL and derives `ws`/`wss` by scheme-swap.
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

## Further reading

- **`PLAN.md`** — server design, decisions, parity notes.
- **`TUI_PLAN.md`** — the `ccs` design and milestones (M0–M5), including the
  emulator choice and the grid.
