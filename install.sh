#!/usr/bin/env bash
# cc-screen-rust installer — the web-only, tmux-free sibling. Runs SIDE-BY-SIDE
# with the Go cc-screen-web (own service name, own config dir, own port 8839).
#
#   ./install.sh                 build + run on the Tailscale IP, port 8839
#   ./install.sh -p 9001         choose the port
#   ./install.sh --bind 0.0.0.0  choose the bind address (default: tailnet IP)
#   ./install.sh --no-build      (re)install the service without rebuilding
#   ./install.sh --no-service    just build the binary, don't run it
#   ./install.sh --no-restore    don't auto-resume sessions at startup
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

while [ $# -gt 0 ]; do
  case "$1" in
    -p|--port)    PORT="$2"; shift 2 ;;
    --port=*)     PORT="${1#*=}"; shift ;;
    -b|--bind)    BIND="$2"; shift 2 ;;
    --bind=*)     BIND="${1#*=}"; shift ;;
    --no-build)   BUILD=0; shift ;;
    --no-service) SERVICE=0; shift ;;
    --no-restore) RESTORE=0; shift ;;
    -h|--help)    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

CFG_DIR="$HOME/.config/cc-screen-rust"
mkdir -p "$CFG_DIR"
printf 'CCWEB_ADDR=%s\n' "$ADDR" > "$CFG_DIR/web.env"

systemd_ok() {
  command -v systemctl >/dev/null 2>&1 &&
    [ -n "${XDG_RUNTIME_DIR:-}" ] &&
    systemctl --user show-environment >/dev/null 2>&1
}

start_systemd() {
  local unit_dir="$HOME/.config/systemd/user"
  mkdir -p "$unit_dir"
  # Bake a PATH with ~/.local/bin so the engine can find the tool binaries
  # (claude, …); the binary also re-prepends it, but be explicit.
  local svc_path="$HOME/.local/bin:$PATH"
  local norestore=""
  [ "$RESTORE" = 1 ] || norestore=" --no-restore"
  cat > "$unit_dir/cc-screen-rust.service" <<EOF
[Unit]
Description=cc-screen-rust — tmux-free phone UI for AI CLIs
After=network-online.target
StartLimitIntervalSec=0

[Service]
Environment=PATH=$svc_path
EnvironmentFile=$CFG_DIR/web.env
# Resume-only model: the agents are in-process PTY children, so a restart ends
# them and auto-restore-on-startup brings them back (resuming each conversation).
# No KillMode tweak needed (unlike the tmux build).
ExecStartPre=/bin/sh -c 'a="\${CCWEB_ADDR}"; ip="\${a%%:*}"; case "\$ip" in ""|0.0.0.0|127.0.0.1|localhost) exit 0;; esac; for i in \$(seq 1 60); do ip -o addr show 2>/dev/null | grep -Fqw "\$ip" && exit 0; sleep 1; done; exit 0'
ExecStart=$BIN$norestore
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
EOF
  systemctl --user daemon-reload
  systemctl --user enable cc-screen-rust.service >/dev/null 2>&1 || true
  systemctl --user restart cc-screen-rust.service
  loginctl enable-linger "$USER" >/dev/null 2>&1 || true
  echo "→ systemd --user service 'cc-screen-rust' running (auto-restart, auto-resume)"
}

if [ "$SERVICE" = 1 ]; then
  if systemd_ok; then
    start_systemd
  else
    echo "→ no systemd --user; start it yourself:  $BIN --addr $ADDR"
  fi
else
  echo "→ built $BIN (not started; --no-service). Run: $BIN --addr $ADDR"
fi

echo
echo "cc-screen-rust is serving on http://$ADDR"
if printf '%s' "$BIND" | grep -q '^100\.'; then
  echo "From a tailnet device, open  http://$ADDR  and Add to Home Screen."
fi
