#!/usr/bin/env bash
# e2e-saas-journey — the whole-journey end-to-end test for the hosted SaaS flow
# (proposal 0053 Part F3): a fresh user, production URLs, under 5 minutes.
#
#   signup → copy-paste one-liner in a clean container → approve the device code
#   → the machine uplinks and shows ONLINE in the dashboard API.
#
# Usage:
#   scripts/e2e-saas-journey.sh                # the full journey (needs docker)
#   scripts/e2e-saas-journey.sh --gate         # the 0053 Part E security-gate probes
#   HUB=https://app.ccscreen.dev SITE=https://ccscreen.dev scripts/e2e-saas-journey.sh
#
# It hits LIVE infra (GitHub release downloads inside the container, Cloudflare),
# so timeouts are generous and every run uses a fresh email — it is re-runnable
# and doubles as a post-deploy smoke test. Never wire it into merge-blocking CI:
# it's a deploy gate, not a PR gate.
#
# Teardown (manual, after a run):
#   docker rm -f e2e-box
#   # and on the hub origin host, delete the throwaway account(s):
#   #   cc-screen-hub user delete "<the e2e+…@ccscreen.dev email the run printed>"
#   # (no cookie-authed account-delete endpoint exists yet — 0056 may add one)
set -eu

HUB="${HUB:-https://app.ccscreen.dev}"
SITE="${SITE:-https://ccscreen.dev}"
MACHINE="${MACHINE:-e2e-box}"
EMAIL="e2e+$(date +%s)@ccscreen.dev"
if command -v openssl >/dev/null 2>&1; then
  PW="$(openssl rand -hex 16)"
else
  PW="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
fi
JAR="$(mktemp)"
BODY="$(mktemp)"
trap 'rm -f "$JAR" "$BODY"' EXIT

say()  { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# POST JSON to $HUB$1 with body $2; extra curl args after that. Prints
# "<status>\n<body>".
post_json() {
  path="$1"; body="$2"; shift 2
  curl -sS -o "$BODY" -w '%{http_code}' -X POST "$HUB$path" \
       -H 'content-type: application/json' -d "$body" "$@" || true
  printf '\n'
  cat "$BODY" 2>/dev/null || true
  : > "$BODY"
}

# ─────────────────────────────────────────────────────────────────────────────
# --gate: probe the 0053 Part E ([0042] P0) items against $HUB. These must PASS
# before the signup URL is promoted publicly (landing page / README). The
# throttle probe runs LAST — it deliberately locks this source IP out of
# signup/login for a while.
# ─────────────────────────────────────────────────────────────────────────────
if [ "${1:-}" = "--gate" ]; then
  gate_rc=0

  # Body "shape": keep the JSON structure + keys, blank out the values, so two
  # responses can be compared for indistinguishability without matching exact
  # message text.
  shape() {
    sed -e 's/:"[^"]*"/:"S"/g' -e 's/:[0-9][0-9.]*/:N/g' \
        -e 's/:true/:B/g' -e 's/:false/:B/g'
  }

  say "gate 1/3: account enumeration — the duplicate-email failure must be generic"
  # A create-account endpoint that succeeds by minting a session cannot make
  # success and duplicate-email byte-identical without an email-verification
  # step (v1 ships no mailer), so the P0 bar (0053 Part E / 0042) is: every
  # server-side signup failure — duplicate email included — answers with ONE
  # generic status + body that never confirms the address exists, and the
  # per-IP throttle (gate 3) bounds probing. Assert exactly that.
  # Setup: mint an account so an "existing email" exists.
  setup="$(post_json /api/signup "{\"email\":\"$EMAIL\",\"password\":\"$PW\"}")"
  setup_status="$(printf '%s' "$setup" | head -1)"
  case "$setup_status" in
    2*) : ;;
    *) fail "gate setup: signup for $EMAIL answered $setup_status — cannot run the enumeration probe
       ($(printf '%s' "$setup" | tail -1))" ;;
  esac
  r_dup="$(post_json /api/signup "{\"email\":\"$EMAIL\",\"password\":\"$PW\"}")"
  r_dup2="$(post_json /api/signup "{\"email\":\"$EMAIL\",\"password\":\"${PW}x\"}")"
  dup_status="$(printf '%s' "$r_dup" | head -1)"
  dup2_status="$(printf '%s' "$r_dup2" | head -1)"
  dup_body="$(printf '%s' "$r_dup" | tail -1)"
  dup_shape="$(printf '%s' "$r_dup" | tail -1 | shape)"
  dup2_shape="$(printf '%s' "$r_dup2" | tail -1 | shape)"
  gate1_ok=1
  # (a) stable: two duplicate attempts (different passwords) answer identically
  [ "$dup_status" = "$dup2_status" ] && [ "$dup_shape" = "$dup2_shape" ] || gate1_ok=0
  # (b) generic: the body never linguistically confirms the address exists
  if printf '%s' "$dup_body" | grep -qiE "exists|in use|taken|already registered|duplicate"; then
    gate1_ok=0
  fi
  if [ "$gate1_ok" = 1 ]; then
    echo "PASS: duplicate-email failure is the generic server-failure answer (status $dup_status), no existence wording"
  else
    echo "FAIL: duplicate-email signup is distinguishable from the generic failure:"
    echo "  attempt 1 → $dup_status  $dup_body"
    echo "  attempt 2 → $dup2_status  $(printf '%s' "$r_dup2" | tail -1)"
    gate_rc=1
  fi
  echo "(teardown: the probe created account $EMAIL)"

  say "gate 2/3: password policy — a 9-char password on public signup must be rejected"
  weak="e2e+$(date +%s)-weak@ccscreen.dev"
  r_weak="$(post_json /api/signup "{\"email\":\"$weak\",\"password\":\"abcdefghi\"}")"
  weak_status="$(printf '%s' "$r_weak" | head -1)"
  case "$weak_status" in
    4*) echo "PASS: short password rejected ($weak_status)" ;;
    *)  echo "FAIL: short (9-char) password answered $weak_status — below the hub's own 12-char bar"
        echo "  (teardown: this may have created account $weak)"
        gate_rc=1 ;;
  esac

  say "gate 3/3: per-IP throttle — a burst of bad signups from one IP must hit 429 (runs last: it locks this IP out for a while)"
  throttled=""
  i=1
  while [ "$i" -le 30 ]; do
    st="$(post_json /api/signup "{\"email\":\"e2e-burst-$i@ccscreen.dev\",\"password\":\"x\"}" | head -1)"
    if [ "$st" = "429" ]; then throttled="yes after $i attempts"; break; fi
    i=$((i + 1))
  done
  if [ -n "$throttled" ]; then
    echo "PASS: signup burst throttled ($throttled)"
  else
    echo "FAIL: 30 failing signups from one IP and never a 429"
    gate_rc=1
  fi

  [ "$gate_rc" -eq 0 ] && echo && echo "GATE OK against $HUB" || { echo; echo "GATE FAILED against $HUB"; }
  exit "$gate_rc"
fi

# ─────────────────────────────────────────────────────────────────────────────
# The journey. Requires docker + the prod URLs reachable.
# ─────────────────────────────────────────────────────────────────────────────
command -v docker >/dev/null 2>&1 || fail "docker is required for the journey (use --gate for the probe-only mode)"
docker rm -f "$MACHINE" >/dev/null 2>&1 || true

say "0/4: landing page $SITE is up and carries the signup link (0054's surface)"
curl -fsS "$SITE" | grep -q "app.ccscreen.dev" \
  || fail "$SITE does not mention app.ccscreen.dev"

say "1/4: signup $EMAIL (mints the identity cookie)"
curl -fsS -c "$JAR" -X POST "$HUB/api/signup" -H 'content-type: application/json' \
     -d "{\"email\":\"$EMAIL\",\"password\":\"$PW\"}" | grep -q '"ok":true' \
  || fail "signup did not answer {\"ok\":true}"

say "2/4: the copy-paste one-liner on a clean box (containerized Linux leg)"
# The installer downloads the agent binary from the GitHub release, prints the
# device code, and waits for approval. The trailing `sleep 600` keeps the box
# alive whatever the installer exits with (its final service step needs systemd,
# which a container doesn't have — the agent is started by hand in step 4).
docker run -d --name "$MACHINE" debian:stable-slim sh -c \
  "apt-get update -qq && apt-get install -y -qq curl ca-certificates procps xz-utils >/dev/null \
   && curl -fsSL '$HUB/install.sh' | sh -s -- '$MACHINE'; sleep 600" >/dev/null

say "3/4: scrape the device code the installer prints, approve it as $EMAIL"
# Code shape per crates/hub/src/db.rs gen_user_code(): XXXX-XXXX from
# [23456789ABCDEFGHJKMNPQRSTVWXYZ]. Generous timeout: apt + the GitHub download.
CODE="$(timeout 240 sh -c "until docker logs '$MACHINE' 2>&1 | grep -oE '[A-Z0-9]{4}-[A-Z0-9]{4}' | head -1 | grep .; do sleep 3; done")" \
  || fail "no device code appeared in the container logs within 240s (docker logs $MACHINE)"
echo "   code: $CODE"
curl -fsS -b "$JAR" -X POST "$HUB/api/device/approve" -H 'content-type: application/json' \
     -d "{\"user_code\":\"$CODE\"}" | grep -q '"machine_id"' \
  || fail "device approve did not answer {\"machine_id\":…}"

say "4/4: the machine uplinks and shows ONLINE in the dashboard API (GET /api/agents)"
# A container has no systemd, so the installer's service step can't start the
# agent — start it by hand with the enrolled token (persisted in enroll.json;
# picked up automatically, see src/enroll.rs).
sleep 5   # let the installer finish persisting enroll.json after approval
docker exec -d "$MACHINE" sh -c \
  "\$HOME/.local/bin/cc-screen-rust --hub '$HUB' --machine-id '$MACHINE' --hub-only >/tmp/agent.log 2>&1"
# /api/agents rows serialize keys alphabetically: …"machine":"…","missing":[…],"online":…
timeout 180 sh -c "until curl -fsS -b '$JAR' '$HUB/api/agents' \
    | grep -o '\"machine\":\"$MACHINE\"[^}]*' | grep -q '\"online\":true'; do sleep 3; done" \
  || fail "'$MACHINE' never showed online in $HUB/api/agents within 180s (docker exec $MACHINE cat /tmp/agent.log)"

echo
echo "JOURNEY OK: $EMAIL -> $MACHINE online at $HUB"
echo
echo "Manual last mile (the product promise is mobile-first): open $HUB in a"
echo "phone-sized viewport, log in as $EMAIL, and confirm the machine + a live"
echo "session render. Windows leg: ssh harebell, then"
echo "  irm \"$HUB/install.ps1?name=harebell-e2e&assistants=all\" | iex"
echo
echo "Teardown:"
echo "  docker rm -f $MACHINE"
echo "  # on the hub origin host:  cc-screen-hub user delete '$EMAIL'"
