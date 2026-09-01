#!/usr/bin/env bash
# Proposal 0088: isolated real-agent/OpenCode lifecycle smoke.
#
# Uses an already-installed OpenCode binary but gives both cc-screen and OpenCode
# a throwaway HOME/XDG tree. It never reads production credentials, contacts a
# model provider, updates the CLI, or changes the operator's normal installation.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
BIN=${CCSCREEN_BIN:-$ROOT/target/release/cc-screen-rust}
if [[ ! -x "$BIN" ]]; then
  echo "missing agent binary: $BIN (build with: cargo build --release -p cc-screen-rust)" >&2
  exit 1
fi

OPENCODE=${OPENCODE_BIN:-$(command -v opencode 2>/dev/null || true)}
if [[ -z "$OPENCODE" || ! -x "$OPENCODE" ]]; then
  echo "missing opencode (set OPENCODE_BIN to an existing binary)" >&2
  exit 1
fi
"$OPENCODE" --version >/dev/null

TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/cc-screen-opencode-e2e.XXXXXX")
HOME_DIR=$TMP_ROOT/home
PROJECT=$HOME_DIR/project
mkdir -p "$PROJECT" "$HOME_DIR/.config" "$HOME_DIR/.local/bin"
PORT=${CCSCREEN_E2E_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}
BASE="http://127.0.0.1:$PORT"
LOG=$TMP_ROOT/agent.log
AGENT_PID=

cleanup() {
  if [[ -n "${AGENT_PID:-}" ]]; then
    curl -sS -X POST -H 'Content-Type: application/json' \
      -d '{"session":"opencode-e2e","mode":"kill"}' \
      "$BASE/api/session/delete" >/dev/null 2>&1 || true
    kill "$AGENT_PID" >/dev/null 2>&1 || true
    wait "$AGENT_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT INT TERM

OPENCODE_DIR=$(cd "$(dirname "$OPENCODE")" && pwd)
PATH_FOR_TEST="$HOME_DIR/.local/bin:$OPENCODE_DIR:/usr/local/bin:/usr/bin:/bin"
env \
  -u CCWEB_HUB_URL -u CCWEB_HUB_TOKEN -u CCWEB_MACHINE_ID -u CCWEB_HUB_ONLY \
  -u CCSCREEN_CONFIG -u CCWEB_PASSWORD -u CCWEB_API_TOKEN \
  HOME="$HOME_DIR" XDG_CONFIG_HOME="$HOME_DIR/.config" CCWEB_HOME="$HOME_DIR" \
  PATH="$PATH_FOR_TEST" \
  "$BIN" --addr "127.0.0.1:$PORT" --no-restore >"$LOG" 2>&1 &
AGENT_PID=$!

for _ in $(seq 1 100); do
  if curl -fsS "$BASE/api/tools" >"$TMP_ROOT/tools.json" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if [[ ! -s "$TMP_ROOT/tools.json" ]]; then
  echo "agent did not become ready" >&2
  cat "$LOG" >&2
  exit 1
fi
python3 - "$TMP_ROOT/tools.json" <<'PY'
import json, sys
rows = json.load(open(sys.argv[1]))
row = next((x for x in rows if x.get("prefix") == "opencode"), None)
assert row is not None, rows
assert row.get("cmd") == "oc", row
assert not row.get("unavailable", False), row
assert "extraDirs" not in row and not row.get("remoteControlAvailable", False), row
PY

CREATE_JSON=$(python3 - "$PROJECT" <<'PY'
import json, sys
print(json.dumps({
  "tool": "oc", "name": "e2e", "dir": sys.argv[1], "extraDirs": [],
  "skipPermissions": True, "assistantRemoteControl": False,
}))
PY
)
curl -fsS -X POST -H 'Content-Type: application/json' -d "$CREATE_JSON" \
  "$BASE/api/session" >"$TMP_ROOT/create.json"
python3 - "$TMP_ROOT/create.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1]))["name"] == "opencode-e2e"
PY

# Let the real TUI reach its composer. OpenCode v1.18.22 needs `/exit` and Enter
# split by the agent; a ready session should therefore restart well inside the
# 10-second forced-kill budget.
sleep 7
START_MS=$(date +%s%3N)
curl -fsS -X POST -H 'Content-Type: application/json' \
  -d '{"session":"opencode-e2e"}' "$BASE/api/session/restart" >"$TMP_ROOT/restart.json"
ELAPSED_MS=$(( $(date +%s%3N) - START_MS ))
python3 - "$TMP_ROOT/restart.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1]))
assert row["session"] == "opencode-e2e", row
assert row["tool"] == "opencode", row
assert row["state"] == "resumed", row
PY
if (( ELAPSED_MS >= 5000 )); then
  echo "OpenCode restart took ${ELAPSED_MS}ms (expected graceful exit, not escalation)" >&2
  cat "$LOG" >&2
  exit 1
fi

curl -fsS "$BASE/api/sessions" >"$TMP_ROOT/sessions.json"
python3 - "$TMP_ROOT/sessions.json" <<'PY'
import json, sys
rows = json.load(open(sys.argv[1]))
assert any(x.get("name") == "opencode-e2e" and x.get("tool") == "opencode" for x in rows), rows
PY

printf 'ok: OpenCode %s · create + graceful restart/resume in %sms\n' \
  "$("$OPENCODE" --version | head -1)" "$ELAPSED_MS"
