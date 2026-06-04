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

The hub is the front door: run one hub, point every machine's agent at it, and
reach all of them from one address. Three pieces, in order.

**① The hub — your front door.** Its own binary + service (default port 8840);
it's the address you open and the clients connect to:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen-hub.sh | sh
cc-screen-hub install --password PW --agents 'laptop:T1,server:T2'
# cc-screen-hub uninstall          # tear the service back down
```

**② The machines — headless hosts.** On each computer where your coding agents
live, install the agent and point it at the hub. `--hub-only` keeps it a pure host:
it runs the agents and dials out, with no inbound and nothing to open directly.

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen.sh | sh
cc-screen-rust install --hub https://hub:8840 --hub-token T1 --machine-id laptop --hub-only
# cc-screen-rust uninstall         # tear the service back down
# cc-screen-rust install --help    # all flags
```

One machine? Run the hub and the host on the same box.

**③ The clients — point them at the hub.** The web app is served by the hub (open
it and Add to Home Screen); `ccs` is the native terminal client:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://cc-screen-b4687da9.dibbla.app/dl/install-ccs.sh | sh
#   browser → https://hub:8840
#   ccs --server https://hub:8840 --token <client-token>
```

See **[HUB.md](HUB.md)** for the full guide (per-agent uplink tokens, the security
model, TLS for off-tailnet). The agent keeps owning its PTYs, so a hub restart
never kills sessions. (A single agent can also serve directly — drop the hub flags
and open `http://machine:8839` — but the hub is the front door for everything.)
Building from source (`./install.sh`) delegates the service step to that same
`cc-screen-rust install`, so the unit/plist has a single source of truth.

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
internet. Clients connect to the hub, so turn the gate on **there**:

```sh
cc-screen-hub install --password 'your-passphrase' --token '<client-token>'
```

The web UI then shows a login screen; a correct password (or the token) sets a
**2-week session cookie**. For the `ccs` TUI, drop the token into
`~/.config/cc-screen-tui/config.toml` as `api_token = "…"` (or pass `ccs --token`,
or set `CCS_API_TOKEN`). This client gate is **separate** from how agents
authenticate to the hub — those use per-agent uplink tokens
(`--agents 'machine:token,…'` on the hub, `--hub-token` on the agent), so a leaked
client password can't impersonate a machine. A standalone agent (no hub) takes the
same `--password`/`--token` via `cc-screen-rust install`; secrets live in each
tool's `web.env` and survive re-running `install`. See [HUB.md](HUB.md).

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
