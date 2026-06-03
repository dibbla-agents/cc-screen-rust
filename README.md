# cc-screen-rust

A **web-only, tmux-free** backend for driving AI coding CLIs
(claude / kimi / gemini / codex) from a phone — a Rust rewrite of cc-screen's
`web/` daemon. The React PWA is reused nearly unchanged; tmux is replaced by an
in-process PTY session engine.

See **[PLAN.md](PLAN.md)** for the design, decisions, and milestones, and
**[HUB.md](HUB.md)** to run one address in front of many machines.

## Status

**Full parity (M1–M6), deployed.** Terminal core (create/attach/type/kill,
key/paste, clear-history, favourites), graceful `exit` vs `kill`, session
persistence + restore (resume-only: a restart ends the agents and auto-restore
resumes each conversation), the `$HOME`-confined files/editor/upload block, and
clipboard image-paste — all working against the real React PWA. Runs as the
`cc-screen-rust` systemd --user service on port 8839, side-by-side with the Go
`cc-screen-web` on 8838. `/api/download` supports HTTP Range; `POST /api/session`
confines the dir + extra dirs to `$HOME`. No known feature or protocol
divergence from the Go app — see PLAN.md "Parity notes".

## Build & run

```sh
./build.sh build          # frontend -> embed -> ./target/release/cc-screen-rust
./build.sh run            # build + run in the foreground
CCWEB_ADDR=127.0.0.1:8839 ./target/release/cc-screen-rust
# or: ./target/release/cc-screen-rust --addr 0.0.0.0:8839
```

Requires the Rust toolchain (`rustup`) and Node (for the Vite build).

## Install

Both binaries ship as prebuilt artifacts (macOS arm64/x86_64, Linux arm64/x86_64
static musl), cross-built by `dist` (`.github/workflows/release.yml`, config in
`dist-workspace.toml`) and **hosted on the cc-screen site itself** (under `/dl`),
so install runs off our own domain — no GitHub account in the path. Each installer
detects OS/arch, verifies the embedded SHA-256 checksum, and drops the binary into
`~/.local/bin`.

**The `ccs` client** — install on any Mac or Linux box on your tailnet:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-ccs.sh | sh
```

**The server** — install the binary, then wire up the service (systemd on Linux,
launchd on macOS) with the binary's own `install` subcommand:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen.sh | sh
cc-screen-rust install            # --port N / --bind ADDR / --no-restore; defaults to the tailnet IP:8839
# cc-screen-rust uninstall        # tear the service back down
# cc-screen-rust install --help   # all flags, including slave/hub mode
```

Building from source instead (`./install.sh`) delegates the service step to that
same `cc-screen-rust install`, so the unit/plist has a single source of truth.

### Many machines: the hub

To put **one address in front of all your machines**, run a **hub**
(`cc-screen-hub`): each machine's agent dials out and registers, and you point
your browser / `ccs` at the hub to see every machine's sessions in one list. See
**[HUB.md](HUB.md)** for the full guide; the short version:

```sh
# on the hub box (its own binary + service, default port 8840):
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen-hub.sh | sh
cc-screen-hub install --password PW --agents 'laptop:T1,server:T2'

# on each machine, point its agent at the hub ("slave" mode):
cc-screen-rust install --hub https://hub:8840 --hub-token T1 --machine-id laptop

# then, from anywhere:
#   browser → https://hub:8840   (or)   ccs --server https://hub:8840 --token <client-token>
```

The agent keeps owning its PTYs (a hub restart never kills sessions) and, unless
`--hub-only`, still serves directly on its tailnet too. `cc-screen-hub --help` and
`cc-screen-hub install --help` explain every flag.

### Updating

Every binary self-updates by re-running its hosted installer (and the services
restart onto the new build):

```sh
cc-screen-rust update     # agent: fetch the latest + restart the service
cc-screen-hub  update     # hub:   fetch the latest + restart the service
ccs            update     # TUI:   fetch the latest ccs binary
```

### Password protection (optional)

Auth is **off by default** — it's tailnet-only, so the gate is just basic
protection against *other* people on your Tailscale network, not the public
internet. Turn it on at install time:

```sh
cc-screen-rust install --password 'your-passphrase'
# → also auto-generates an API token (printed once) for the ccs TUI
```

The web UI then shows a login screen; a correct password (or the token) sets a
**2-week session cookie**. For the `ccs` TUI, drop the printed token into
`~/.config/cc-screen-tui/config.toml` as `api_token = "…"` (or pass `ccs --token`,
or set `CCS_API_TOKEN`). Both secrets live in `~/.config/cc-screen-rust/web.env`
(`CCWEB_PASSWORD`, `CCWEB_API_TOKEN`) and can be edited there; re-running
`install` preserves them.

**Cutting a release.** Bump the version with `./bump.sh X.Y.Z` (lockstep across the
three crates + `Cargo.lock`) and commit it; then `./release.sh` tags + waits for
the CI cross-build (which also publishes a GitHub Release as a mirror); then
`site/release-host.sh` re-hosts those CI artifacts on the site under `/dl` —
exactly what the one-liners above serve. (The `/release` skill walks an agent
through the whole thing, including updating the running server vs. the docs site.)

## Docs site

The getting-started site lives in **`docs/`** (one source of truth) and is
deployed through **Dibbla** as the `cc-screen` app — a tiny Rust static server
(`site/`, axum + tower-http) in a small non-root Alpine container:

- **Live:** https://cc-screen-b4687da9.dibbla.app
- **Deploy / update:** `cd site && ./deploy.sh` — syncs `docs/` → `site/public`,
  then `dibbla deploy --alias cc-screen` (first run creates the app; later runs do
  a zero-downtime `--update`). See `site/README.md`.

(`docs/` is also served by GitHub Pages at
https://dibbla-agents.github.io/cc-screen-rust/.)

## Layout

| Path | What |
|------|------|
| `src/main.rs` | axum router, static embed, startup, `--help`, `install`/`uninstall` |
| `src/config.rs` | paths (`~/.config/cc-screen-rust/`), bind addr, tool-registry + hub-flag resolution |
| `src/tools.rs` | `tools.conf` parsing + launch/resume command building (port of the Go `tmux.go` registry) |
| `src/engine.rs` | the session engine: `Session` (PTY + vt100 + ring + broadcast), `AppState`, spawn/pump/reap |
| `src/handlers.rs` | HTTP + WebSocket handlers (the existing frontend's wire contract) |
| `src/attach.rs` | the transport-agnostic attach loop (drives both the local WS handler and the hub uplink) |
| `src/uplink.rs` | the agent→hub uplink (slave mode): dial out, register, relay terminals/files/watch |
| `crates/hub/` | **`cc-screen-hub`** — the aggregator binary (registry + relay; see [HUB.md](HUB.md)) |
| `crates/protocol/` | shared HTTP+WS wire types + the agent↔hub envelope (`src/hub.rs`) |
| `crates/auth/`, `crates/push/` | shared auth (signed cookies/tokens) + Web Push, used by both agent and hub |
| `crates/tui/` | **`ccs`** — the native terminal client |
| `frontend/` | copy of the cc-screen React PWA with minimal tmux-decoupling patches |

## Relationship to cc-screen

This runs **side-by-side** with the Go app: its own config dir and port, reusing
the `tools.conf` format. It is **tailnet-only** by design (the agents launch with
`--dangerously-skip-permissions`/YOLO) — never bind a public interface.
