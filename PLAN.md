# cc-screen-rust — plan

A **web-only, tmux-free** rewrite of cc-screen's web backend in Rust. The React
PWA frontend is reused nearly unchanged; the Go+tmux backend is replaced by an
in-process session engine.

## Decisions (locked)

- **Restart model: resume-only.** A backend restart/crash ends the agent
  processes (they are in-process PTY children); on restart they're relaunched
  and each CLI's own resume flag (`claude --continue`, `codex resume --last`,
  `gemini --resume latest`, `kimi --continue`) reloads the conversation from
  disk. Auto-restore-on-startup (M3) makes a redeploy recover automatically,
  from the last *saved turn* (not mid-operation). Upgrade path kept open: PTY
  ownership is behind the engine so a future "survive graceful restart"
  (systemd FDSTORE) can slot in.
- **v1 scope: full parity** with the Go web app (terminal, files/editor/PDF,
  upload, favourites, browse, clipboard image-paste, multi-pane, restore).
- **Deployment: side-by-side.** Own config dir `~/.config/cc-screen-rust/`, own
  port (default **8839**), reusing the `tools.conf` *format* (and the existing
  shared file if present) but a separate session store.

## Architecture: the session engine (replaces tmux)

Each `Session` owns its PTY master for the whole session lifetime — that's what
lets input work with no client attached, and what a WebSocket attaches to. A
blocking reader thread pumps PTY output into three sinks:

1. **vt100 parser** — authoritative screen model → the session-list preview line.
2. **bounded raw-byte ring** (~768 KB) — replayed (after a `\x1bc` reset) on
   every (re)attach so a reconnecting xterm.js re-parses the exact same bytes
   and repaints correctly, *including* alt-screen/mouse/bracketed-paste.
3. **broadcast channel** — live fan-out to attached WS clients.

The broadcast send happens *inside* the state lock; `attach()` snapshots the
ring + subscribes under the same lock, so no byte is ever both replayed and
streamed (and none is missed).

### Emulator choice

`portable-pty` (wezterm) for the PTY; `vt100` for the headless screen model.
xterm.js on the client is the real renderer (via raw-byte replay), so the server
emulator only needs the preview line + size tracking — vt100 is right-sized;
`alacritty_terminal` is the upgrade if we ever need server-side scrollback
snapshotting.

## Frontend reuse — patches applied

Copied `web/frontend` → `frontend/`. Only divergence so far:

1. `TerminalView.tsx` swipe-scroll → `term.scrollLines()` (client-side) instead
   of the tmux-only `{t:"s"}` round-trip.

Reattach repaint needs no frontend change: the server prefixes its snapshot with
`\x1bc` (RIS), which resets xterm before the replay. `clear-history` is a real
server op (keeps the visible screen, drops scrollback).

## Milestones — all complete (full parity)

- **M1 — engine + terminal core** ✅ create/attach/type/kill, key/paste,
  clear-history, tools, session/root, favourites, static embed.
- **M2 — graceful exit** ✅ `/api/session/delete` mode `exit` injects the agent's
  `/exit` (202); `kill` tears down (204); both forget the manifest entry.
- **M3 — persistence/restore** ✅ manifest at `sessions.json`; the reaper observes
  each child's exit status (clean exit → forget; crash/redeploy → keep);
  `restorable`/`restore`; auto-restore on startup; per-tool resume specs with
  `(resume) || (launch)` fallback. No exit-markers / reconcile needed — the
  engine owns every session, so in-process exit status is authoritative.
- **M4 — files/editor/upload** ✅ the `$HOME`-confined HTTP block: `dirs`,
  `files`, `download` (streamed, inline/attachment), `file/read|write|delete`,
  `mkdir|rmdir|rename`, `upload`(+`check`). Multipart preserves folder relpaths;
  collisions rename. `confine.rs` mirrors resolveUnderHome/Root/safeRel.
- **M5 — clipboard image-paste** ✅ `clip` + `clip/targets` + `clip/image.png`
  (TTL store; writes Ctrl-V to the PTY). Shim must point at this server's
  `web.env` to use it under side-by-side.
- **M6 — hardening** ✅ Rust tests (confine/tools/manifest unit + a real-PTY
  engine test — no tmux), `install.sh` (systemd --user, :8839), deployed.

### Parity notes
- `/api/download` supports single-range `Range:` requests (206 + Content-Range +
  Accept-Ranges), matching Go's `http.ServeFile`; multi-range falls back to 200.
- `POST /api/session` confines `dir` to `$HOME`, validates+dedupes `extraDirs`
  (and enforces the tool max), and defaults a blank name to the dir's basename.
- Favourites validation matches the Go store (require id, dedupe by id, cap
  8000 chars / 200 entries). Stored in `~/.config/cc-screen-rust/` (side-by-side).
- No HTTP-protocol or feature divergence from the Go web app is known. The only
  intentional differences are tmux-specific things that don't apply web-only:
  client-side swipe-scroll, and no on-attach scrollback-wipe (each client has
  its own xterm; the ring-replay handles reattach).

## Known v1 limitations / risks

- Every redeploy interrupts agents (resume-only choice); auto-restore recovers
  them from the last saved turn.
- Reattach scrollback is bounded by the ring; history older than the ring isn't
  replayed on reconnect.
- The fragile spot is a mid-sequence cut at the ring head; mitigated by the
  `\x1bc` reset prefix + a generous ring. Validate against real Claude/Codex
  alt-screen sessions early.
