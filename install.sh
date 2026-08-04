#!/usr/bin/env bash
# cc-screen-rust installer — build this machine's agent from source + wire up the
# service. Runs SIDE-BY-SIDE with the Go cc-screen-web (own service, config dir,
# port 8839). Service setup is delegated to `cc-screen-rust install`.
#
#   ./install.sh                      build + run on the Tailscale IP, port 8839
#   ./install.sh -p 9001              choose the port
#   ./install.sh --bind 0.0.0.0       choose the bind address (default: tailnet IP)
#   ./install.sh --no-build           (re)install the service without rebuilding
#   ./install.sh --no-service         just build the binary, don't run it
#   ./install.sh --no-restore         don't auto-resume sessions at startup
#   ./install.sh --assistants[=LIST]  also install the missing coding-assistant CLIs
#                                     for your user (~/.local, no sudo); LIST narrows it
#
# Slave mode — also register with a hub (see HUB.md / `cc-screen-hub install`):
#   ./install.sh --hub URL --hub-token TOK --machine-id NAME [--hub-only]
#
# tailnet-only by design: the agents launch with --dangerously-skip-permissions.
set -euo pipefail
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

PORT=8839
BIND=""
BUILD=1
SERVICE=1
RESTORE=1
HUB=""
HUB_TOKEN=""
MACHINE_ID=""
HUB_ONLY=0
ASSISTANTS="${CCSCREEN_ASSISTANTS:-}"

while [ $# -gt 0 ]; do
  case "$1" in
    -p|--port)      PORT="$2"; shift 2 ;;
    --port=*)       PORT="${1#*=}"; shift ;;
    -b|--bind)      BIND="$2"; shift 2 ;;
    --bind=*)       BIND="${1#*=}"; shift ;;
    --no-build)     BUILD=0; shift ;;
    --no-service)   SERVICE=0; shift ;;
    --no-restore)   RESTORE=0; shift ;;
    --hub)          HUB="$2"; shift 2 ;;
    --hub=*)        HUB="${1#*=}"; shift ;;
    --hub-token)    HUB_TOKEN="$2"; shift 2 ;;
    --hub-token=*)  HUB_TOKEN="${1#*=}"; shift ;;
    --machine-id)   MACHINE_ID="$2"; shift 2 ;;
    --machine-id=*) MACHINE_ID="${1#*=}"; shift ;;
    --hub-only)     HUB_ONLY=1; shift ;;
    --assistants)   ASSISTANTS="all"; shift ;;
    --assistants=*) ASSISTANTS="${1#*=}"; shift ;;
    --no-assistants) ASSISTANTS=""; shift ;;
    -h|--help)      sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

if [ -z "$BIND" ]; then
  BIND="$(tailscale ip -4 2>/dev/null | head -1 || true)"
  [ -z "$BIND" ] && BIND=127.0.0.1
fi
ADDR="$BIND:$PORT"

if [ "$BUILD" = 1 ]; then
  command -v npm >/dev/null 2>&1 || { echo "npm not found; cannot build the UI." >&2; exit 1; }
  ( cd frontend && { [ -d node_modules ] || npm ci; } && npm run build )
  command -v cargo >/dev/null 2>&1 || { echo "cargo not found; cannot build the server." >&2; exit 1; }
  cargo build --release
fi

BIN="$(pwd)/target/release/cc-screen-rust"
[ -x "$BIN" ] || { echo "binary not found: $BIN — run without --no-build first." >&2; exit 1; }

# Preflight the assistant CLIs the agent drives (claude/codex/gemini/kimi).
# Best-effort: unlike the npm/cargo guards above (build deps, hard exit), a
# missing assistant only limits which session types work — never abort over it.
# Interactive runs get an offer to install each missing one; piped runs just get
# the report + install one-liners. The list lives in the binary (single source
# of truth), so this script stays thin.
# `--assistants` (proposal 0050) installs them non-interactively, for this user
# only (~/.local, no sudo) — the same runner the web action uses.
case "$ASSISTANTS" in
  "")
    if [ -t 0 ]; then
      "$BIN" doctor --install || true
    else
      "$BIN" doctor || true
    fi
    ;;
  all) "$BIN" doctor --install --yes || true ;;
  *)   "$BIN" doctor --install --yes --only "$ASSISTANTS" || true ;;
esac

# Service setup now lives in the binary itself (`cc-screen-rust install`), so the
# systemd-unit / launchd-plist logic has a single source of truth and works on
# macOS too. The subcommand writes ~/.config/cc-screen-rust/web.env, (re)starts
# the service, and prints the serving URL + tailnet hint.
if [ "$SERVICE" = 1 ]; then
  set -- install --bind "$BIND" --port "$PORT"
  [ "$RESTORE" = 1 ] || set -- "$@" --no-restore
  [ -n "$HUB" ]        && set -- "$@" --hub "$HUB"
  [ -n "$HUB_TOKEN" ]  && set -- "$@" --hub-token "$HUB_TOKEN"
  [ -n "$MACHINE_ID" ] && set -- "$@" --machine-id "$MACHINE_ID"
  [ "$HUB_ONLY" = 1 ]  && set -- "$@" --hub-only
  "$BIN" "$@"
else
  CFG_DIR="$HOME/.config/cc-screen-rust"
  mkdir -p "$CFG_DIR"
  printf 'CCWEB_ADDR=%s\n' "$ADDR" > "$CFG_DIR/web.env"
  # Image-paste shim still gets wired even when we don't start the service, so a
  # foreground run has working Ctrl-V paste (the service path does this inside
  # `cc-screen-rust install`).
  "$BIN" install-shim || echo "→ warning: clipboard shim not installed" >&2
  echo "→ built $BIN (not started; --no-service). Run: $BIN --addr $ADDR"
fi
