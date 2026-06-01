# cc-screen-rust

A **web-only, tmux-free** backend for driving AI coding CLIs
(claude / kimi / gemini / codex) from a phone — a Rust rewrite of cc-screen's
`web/` daemon. The React PWA is reused nearly unchanged; tmux is replaced by an
in-process PTY session engine.

See **[PLAN.md](PLAN.md)** for the design, decisions, and milestones.

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

## Layout

| Path | What |
|------|------|
| `src/main.rs` | axum router, static embed, startup |
| `src/config.rs` | paths (`~/.config/cc-screen-rust/`), bind addr, tool-registry resolution |
| `src/tools.rs` | `tools.conf` parsing + launch/resume command building (port of the Go `tmux.go` registry) |
| `src/engine.rs` | the session engine: `Session` (PTY + vt100 + ring + broadcast), `AppState`, spawn/pump/reap |
| `src/handlers.rs` | HTTP + WebSocket handlers (the existing frontend's wire contract) |
| `frontend/` | copy of the cc-screen React PWA with minimal tmux-decoupling patches |

## Relationship to cc-screen

This runs **side-by-side** with the Go app: its own config dir and port, reusing
the `tools.conf` format. It is **tailnet-only** by design (the agents launch with
`--dangerously-skip-permissions`/YOLO) — never bind a public interface.
