#!/bin/sh
# cc-screen-rust — install this machine and connect it to a cc-screen hub.
#
# This script is SERVED BY THE HUB at /install.sh with the hub URL baked in, so
# the only thing you supply is a name for this machine:
#
#     curl -fsSL <hub>/install.sh | sh -s -- <machine-name>
#
# It (1) installs the cc-screen-rust binary (macOS arm64/x64 + Linux, auto-
# detected), (2) enrolls this machine with the hub — a short code appears that
# you approve from the dashboard — and (3) installs it as a background service
# that reconnects on boot. Re-running is safe (idempotent).
set -eu

HUB_URL="__CCSCREEN_HUB_URL__"
INSTALLER_URL="__CCSCREEN_INSTALLER_URL__"
# Machine name: first argument, else this host's short name.
MACHINE="${1:-$(hostname 2>/dev/null | cut -d. -f1)}"
BIN="$HOME/.local/bin/cc-screen-rust"

if ! command -v curl >/dev/null 2>&1; then
  echo "error: curl is required" >&2
  exit 1
fi

echo "==> Installing the cc-screen-rust binary…"
curl -fsSL "$INSTALLER_URL" | sh

# The cargo-dist installer drops the binary in ~/.local/bin; fall back to PATH.
[ -x "$BIN" ] || BIN="$(command -v cc-screen-rust 2>/dev/null || echo "$HOME/.local/bin/cc-screen-rust")"

echo
echo "==> Checking which coding assistants are installed…"
# Best-effort preflight (the binary owns the list — see `cc-screen-rust doctor`):
# report which of claude/codex/gemini/kimi this machine has and print the install
# command for each missing one. A `curl | sh` run can't prompt, so this only
# reports; a missing assistant never aborts the machine install — the agent is
# still useful for the CLIs that are present.
if [ -t 0 ]; then
  "$BIN" doctor --install || true
else
  "$BIN" doctor || true
fi

echo
echo "==> Connecting '$MACHINE' to $HUB_URL"
echo "    A code will print below — approve it at $HUB_URL/activate (you must be logged in)."
echo
# One command: device flow (prints a code, waits for dashboard approval, saves the
# token), then installs the background service (--hub-only = reachable only via the
# hub, binds no local port). Reconnects on boot.
if ! "$BIN" install --hub "$HUB_URL" --machine-id "$MACHINE" --hub-only --enroll; then
    echo "install failed: '$MACHINE' enrolled but the reconnect service was not registered." >&2
    exit 1
fi

echo
echo "✓ Done — '$MACHINE' is connected and will reconnect automatically."
