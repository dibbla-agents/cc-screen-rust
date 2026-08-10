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
#   # --gate also leaves an e2e-known-…@ccscreen.test account and the org it
#   # invited into (deleting the owner reaps the org's rows by CASCADE)
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

# GET $HUB$1; extra curl args after that. Same "<status>\n<body>" shape.
get_json() {
  path="$1"; shift
  curl -sS -o "$BODY" -w '%{http_code}' "$HUB$path" "$@" || true
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

  say "gate 1/3: account enumeration — signup AND invite must both stay silent about an address"
  # Two legs, because there are two endpoints that know whether an address has an
  # account. (a) signup, which has always known; and (b) the invite sites, which
  # since 0073 also SEND — and a send is an observable the answer can differ in.
  #
  # (a) A create-account endpoint that succeeds by minting a session cannot make
  # success and duplicate-email byte-identical without an email-verification
  # step. The hub now HAS a mailer (0073) but deliberately ships no verification
  # mail (0073 Non-Goals), so the P0 bar (0053 Part E / 0042) is unchanged: every
  # server-side signup failure — duplicate email included — answers with ONE
  # generic status + body that never confirms the address exists, and the
  # per-IP throttle (gate 3) bounds probing. Assert exactly that.
  # Setup: mint an account so an "existing email" exists.
  setup="$(post_json /api/signup "{\"email\":\"$EMAIL\",\"password\":\"$PW\"}" -c "$JAR")"
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

  # (b) The invite arms. [0056] C2's no-oracle rule used to be cheap to hold:
  # the address with no account got nothing at all, so there was nothing to
  # compare. 0073 makes both arms *send*, which is the stronger property and the
  # riskier one — a mailer that skips the ghost arm (or the known one) turns the
  # invite endpoint into the enumeration oracle this gate exists to catch. So the
  # assertion is now: same response shape AND same mail evidence.
  #
  # "Mail evidence" is read from the OWNER'S OUTBOX (GET /api/orgs/invites), not
  # from the create response — the create response deliberately carries no
  # delivery field at all (0073 D1), and that is itself asserted below. `delivery`
  # is null when the hub has no mailer configured, which is still evidence: what
  # must never happen is the two arms differing.
  #
  # Both probe addresses are on the RFC 2606 reserved TLD `.test` on purpose: a
  # real relay refuses both arms identically — which is precisely the property
  # under test — so a gate run costs the sending domain no bounce and no mailbox
  # anywhere receives a probe.
  gate1b_ok=1
  known="e2e-known-$(date +%s)@ccscreen.test"   # signs up → the "has an account" arm
  ghost="e2e-ghost-$(date +%s)@ccscreen.test"   # never signs up → the invitation's whole point
  org_name="e2e-gate-$(date +%s)"
  r_known_signup="$(post_json /api/signup "{\"email\":\"$known\",\"password\":\"$PW\"}")"
  r_org="$(post_json /api/orgs "{\"name\":\"$org_name\"}" -b "$JAR" -c "$JAR")"
  org_status="$(printf '%s' "$r_org" | head -1)"
  if [ "$(printf '%s' "$r_known_signup" | head -1)" != "200" ] || [ "$org_status" != "200" ]; then
    echo "SKIP: invite-arm leg needs an account + an org (signup $(printf '%s' "$r_known_signup" | head -1), org $org_status) — $(printf '%s' "$r_org" | tail -1)"
  else
    r_inv_known="$(post_json /api/orgs/invites "{\"email\":\"$known\"}" -b "$JAR" -c "$JAR")"
    r_inv_ghost="$(post_json /api/orgs/invites "{\"email\":\"$ghost\"}" -b "$JAR" -c "$JAR")"
    known_status="$(printf '%s' "$r_inv_known" | head -1)"
    ghost_status="$(printf '%s' "$r_inv_ghost" | head -1)"
    known_shape="$(printf '%s' "$r_inv_known" | tail -1 | shape)"
    ghost_shape="$(printf '%s' "$r_inv_ghost" | tail -1 | shape)"
    [ "$known_status" = "$ghost_status" ] && [ "$known_shape" = "$ghost_shape" ] || gate1b_ok=0
    # No delivery field may leak into the create response — that is the field an
    # attacker would read without ever needing the owner's outbox.
    if printf '%s%s' "$r_inv_known" "$r_inv_ghost" | grep -qiE '"(emailed|emailed_at|delivery)"'; then
      gate1b_ok=0
      echo "  (the invite create response carries a delivery field — 0073 D1 says it must not)"
    fi
    # The send is SPAWNED, never awaited, so the receipt lands shortly AFTER the
    # 200: poll the outbox for up to 30s rather than reading it once — reading it
    # immediately would compare two nulls and prove nothing. `delivery_of` pulls
    # one row's value out of the array without needing jq on the operator's box.
    delivery_of() { printf '%s' "$1" | tr '}' '\n' | grep -F "$2" | grep -o '"delivery":[^,}]*' | head -1 || true; }
    known_del=""; ghost_del=""; i=1
    while [ "$i" -le 30 ]; do
      outbox="$(get_json /api/orgs/invites -b "$JAR" | tail -1)"
      known_del="$(delivery_of "$outbox" "$known")"
      ghost_del="$(delivery_of "$outbox" "$ghost")"
      # Keep waiting while either row is unwritten, still null (no attempt has
      # been recorded yet) or mid-flight; a hub with no mailer never leaves that
      # state and simply falls out of the loop with both rows null.
      case "$known_del$ghost_del" in
        ""|*sending*|*null*) : ;;
        *) break ;;
      esac
      sleep 1
      i=$((i + 1))
    done
    if [ "$known_del" != "$ghost_del" ]; then
      gate1b_ok=0
      echo "  (the two arms report DIFFERENT delivery state: known=$known_del ghost=$ghost_del)"
    fi
    if [ "$gate1b_ok" = 1 ]; then
      echo "PASS: both invite arms answer $known_status with the same shape and the same mail evidence ($known_del)"
    else
      echo "FAIL: the invite arms are distinguishable — the mailer is an account-existence oracle:"
      echo "  has an account → $known_status  $(printf '%s' "$r_inv_known" | tail -1)  [$known_del]"
      echo "  no account     → $ghost_status  $(printf '%s' "$r_inv_ghost" | tail -1)  [$ghost_del]"
      gate_rc=1
    fi
    # What the hub says it can do (0073 D1), as context for the line above. A hub
    # advertising mail:true that recorded nothing for EITHER arm is not an oracle
    # — the arms still match — but it is worth naming, because it means no
    # receipt reached the outbox.
    mail_flag="$(get_json /api/me -b "$JAR" | tail -1 | grep -o '"mail":[a-z]*' | head -1 || true)"
    case "$mail_flag:$known_del" in
      '"mail":true:'|'"mail":true:"delivery":null')
        echo "  (NOTE: $HUB reports mail:true but neither arm recorded a delivery within 30s — check 0073 B2)" ;;
      '"mail":false:'*)
        echo "  (this hub has no mail transport: mail:false, both arms null — the copyable link is the only channel)" ;;
      *) : ;;
    esac
  fi
  echo "(teardown: the probe created accounts $EMAIL and $known, and the org \"$org_name\")"

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
