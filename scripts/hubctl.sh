#!/usr/bin/env bash
# hubctl — drive the two cc-screen-hub environments on this host.
#
#   test  → the host-native COMPILED hub on :8840 (where features get tried
#           first). It IS the existing `cc-screen-hub.service` systemd --user
#           unit, whose ExecStart points at this repo's target/release binary —
#           so `test restart` = rebuild + bounce the unit, new code live on 8840
#           in seconds. State: ~/.config/cc-screen-hub (host dir).
#   prod  → the DOCKERIZED hub on host :8841 (stable; the container listens on
#           8840 internally, mapped to host 8841). State lives in the `hub-config`
#           docker volume — isolated from test's host dir. Promoted by REBUILDING
#           the image from current source.
#
# Flow: hack on a feature → `hubctl.sh test restart` → kick the tires on :8840 →
# happy → `hubctl.sh prod promote` to rebuild the :8841 container from that same
# source. Test and prod are fully decoupled; promoting is the only bridge.
#
# Usage:
#   scripts/hubctl.sh test  <build|start|stop|restart|status|logs>
#   scripts/hubctl.sh prod  <up|down|restart|promote|status|logs>
#   scripts/hubctl.sh both  status
#
# Config (override via env or scripts/hubctl.env, which is sourced if present):
#   PROD_PORT=8841   — host port the docker prod hub publishes
#   TEST_PORT=8840   — port the test (systemd) hub binds; cosmetic in status
#   TEST_UNIT=cc-screen-hub.service   — the systemd --user unit test drives
set -euo pipefail
cd "$(dirname "$0")/.."          # repo root
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
[ -f scripts/hubctl.env ] && . scripts/hubctl.env

# ── tunables ─────────────────────────────────────────────────────────────────
# test (compiled) on 8840; prod (docker) on host 8841.
PROD_PORT="${PROD_PORT:-8841}"
TEST_PORT="${TEST_PORT:-8840}"
# The test hub IS the existing `cc-screen-hub.service` systemd --user unit, whose
# ExecStart already points at this repo's target/release/cc-screen-hub. So the
# fast loop is just: rebuild the binary, restart the unit. Its bind + config dir
# live in ~/.config/cc-screen-hub/web.env (tailnet IP : 8840 by default).
TEST_UNIT="${TEST_UNIT:-cc-screen-hub.service}"
COMPOSE=(docker compose -f docker/hub/docker-compose.yml)
export HUB_HOST_PORT="$PROD_PORT"   # the compose file maps ${HUB_HOST_PORT}:8840

die() { echo "hubctl: $*" >&2; exit 1; }
sc() { systemctl --user "$@" "$TEST_UNIT"; }

# ── test env: the host-native compiled build on :8840 (systemd unit) ─────────
test_build() {
  echo "── building cc-screen-hub (release) ──"
  cargo build --release -p cc-screen-hub
}
test_start()   { sc start;   test_status; }
test_stop()    { sc stop;    echo "test hub (8840) stopped"; }
# Fast iteration: rebuild the binary, then bounce the unit so it execs the new one.
test_restart() { test_build; echo "── restarting $TEST_UNIT ──"; sc restart; sleep 0.4; test_status; }
test_status()  {
  if sc is-active --quiet; then
    echo "test  :$TEST_PORT  UP    (systemd: $TEST_UNIT, bin: target/release/cc-screen-hub)"
  else echo "test  :$TEST_PORT  down  (systemd: $TEST_UNIT)"; fi
}
test_logs()    { journalctl --user -u "$TEST_UNIT" -f; }

# ── prod env: the dockerized hub on host :8841 ───────────────────────────────
prod_env_ready() {
  [ -f docker/hub/.env ] || { cp docker/hub/.env.example docker/hub/.env; \
    echo "→ created docker/hub/.env from the example — edit tokens before exposing it"; }
}
prod_up()      { prod_env_ready; "${COMPOSE[@]}" up -d; prod_status; }
prod_promote() { prod_env_ready; echo "── rebuilding + restarting prod from current source ──"; \
                 "${COMPOSE[@]}" up -d --build; prod_status; }
prod_down()    { "${COMPOSE[@]}" down; }
prod_restart() { "${COMPOSE[@]}" restart; }
prod_logs()    { "${COMPOSE[@]}" logs -f; }
prod_status()  {
  if "${COMPOSE[@]}" ps --status running 2>/dev/null | grep -q cc-screen-hub; then
    echo "prod  :$PROD_PORT  UP    (docker: cc-screen-hub)"
  else echo "prod  :$PROD_PORT  down  (docker)"; fi
}

# ── dispatch ─────────────────────────────────────────────────────────────────
env="${1:-}"; cmd="${2:-}"
case "$env" in
  test)
    case "$cmd" in
      build)   test_build ;;
      start)   test_start ;;
      stop)    test_stop ;;
      restart) test_restart ;;
      status)  test_status ;;
      logs)    test_logs ;;
      *) die "test: build|start|stop|restart|status|logs" ;;
    esac ;;
  prod)
    case "$cmd" in
      up)      prod_up ;;
      down)    prod_down ;;
      restart) prod_restart ;;
      promote) prod_promote ;;
      status)  prod_status ;;
      logs)    prod_logs ;;
      *) die "prod: up|down|restart|promote|status|logs" ;;
    esac ;;
  both)
    case "$cmd" in
      status) prod_status; test_status ;;
      *) die "both: status" ;;
    esac ;;
  *) sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
