#!/usr/bin/env bash
# e2e-team — the Team-tier end-to-end test (proposals 0063 E1 / 0064 / 0065 F).
#
# Unlike e2e-billing.sh / e2e-saas-journey.sh (which target live infra), this
# script stands up its OWN local multi-tenant hub under a temp dir — no Stripe,
# no docker, no network beyond loopback — so it is safe to run anywhere the
# repo builds, and it is a PR-gate candidate, not just a deploy gate.
#
#   build cc-screen-hub --features multi-tenant
#   → run it on a free loopback port with CCHUB_DATABASE_URL=sqlite://$TMP/hub.db
#     and CCHUB_MAIL_DIR=$TMP/mail — 0073's capture transport, which writes each
#     invite message to a file instead of sending it, so the mail assertions in
#     step 4 need neither a relay nor a network (loopback stays the only hop)
#   → drive the whole org/membership/visibility/limits lifecycle over HTTP
#   → SQL-assert the rows (sqlite3), including the final CASCADE teardown.
#
# The steps map to 0063 E1 (+ the 0065 Part F visibility legs, + 0073 E1's mail):
#    1. owner signs up, creates the org (dormant, 0 seats)
#    2. machines enrolled for owner + member A (pre-join, via the admin CLI)
#    3. invite member A (existing account) and B (no account) — uniform responses
#       AND uniform mail (0073's capture transport writes both messages to a dir)
#    4. zero-seat accept → 402 ("no seats yet")
#    5. `cc-screen-hub org seats` sets 3 — CLI + SQL + audit assert
#    6. A accepts → member row + member.joined audit row (SQL asserts)
#    7. B signs up via the invite link (public token read carries the verbatim
#       consent copy), accepts via the inbox
#    8. seat-full: third accept → 402; one-org: accept while in another team → 409
#    9. pooled limits in /api/me (team, 10×seats / 50×seats, seats/members/orgId)
#   10. pooled machine gate: hand-tuned team row → device-flow approve 402s with
#       the pool message while a re-enroll of an existing label succeeds
#   11. team visibility: B's received list carries A's machine as a view-only
#       team row; leave/revoke on it → 409; session create on it → 404
#   12. the owner opt-out toggle → machine.visibility_changed audit row + the
#       received list loses/regains the machine
#   13. role/permission matrix: member 403s, non-member 404s, owner-leave 409,
#       admin-vs-owner removal rules, ownership transfer, member removal
#   14. audit log: every expected action present; paging strictly descending
#   15. teardown: DELETE the org → CASCADE reaps members/invites/audit/team-shares
#
# ── Environment (all optional) ──
#   PORT      hub port (default 18873)
#   HUB_BIN   prebuilt hub binary (skips the cargo build)
#   KEEP=1    keep the temp dir + leave the hub running (for debugging)
#
# ── Tooling ── cargo (unless HUB_BIN), curl, jq, sqlite3.
#
# Not covered here (documented gaps, covered elsewhere):
#   - the view-only-vs-use distinction against a LIVE agent (the 404 on session
#     create below also holds for an offline machine; the authoritative
#     scope-predicate tests are the registry/db unit tests in crates/hub);
#   - Stripe seat billing (e2e-billing.sh's team check, STRIPE_* gated);
#   - the Team UI (frontend/tools/smoke-team.mjs).
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-18873}"
HUB="http://127.0.0.1:$PORT"

# ── colours (fall back to plain when not a tty) ─────────────────────────────
if [ -t 1 ]; then
  C_G="$(printf '\033[32m')"; C_R="$(printf '\033[31m')"
  C_Y="$(printf '\033[33m')"; C_0="$(printf '\033[0m')"
else
  C_G=""; C_R=""; C_Y=""; C_0=""
fi

rc=0
say()   { printf '\n== %s\n' "$*"; }
pass()  { printf '%sPASS%s: %s\n' "$C_G" "$C_0" "$*"; }
badfl() { printf '%sFAIL%s: %s\n' "$C_R" "$C_0" "$*"; rc=1; }
die()   { printf '%sFAIL%s: %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }
skip()  { printf '\n%sSKIP%s: %s\n' "$C_Y" "$C_0" "$*"; exit 0; }

for t in curl jq; do
  command -v "$t" >/dev/null 2>&1 || skip "e2e-team needs $t on PATH"
done
# SQL asserts run through the sqlite3 CLI when present, else python3's stdlib
# sqlite3 module (same engine) — one of the two is required.
if command -v sqlite3 >/dev/null 2>&1; then
  SQL_VIA="sqlite3"
elif command -v python3 >/dev/null 2>&1; then
  SQL_VIA="python3"
else
  skip "e2e-team needs sqlite3 or python3 on PATH for the SQL asserts"
fi

# ── build (or take) the multi-tenant hub binary ─────────────────────────────
if [ -z "${HUB_BIN:-}" ]; then
  say "0/15: build cc-screen-hub --features multi-tenant"
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  command -v cargo >/dev/null 2>&1 || skip "cargo not on PATH and no HUB_BIN given"
  (cd "$REPO" && cargo build -q -p cc-screen-hub --features multi-tenant) \
    || die "cargo build -p cc-screen-hub --features multi-tenant failed"
  HUB_BIN="$REPO/target/debug/cc-screen-hub"
fi
[ -x "$HUB_BIN" ] || die "hub binary not executable: $HUB_BIN"

# ── isolated hub instance under a temp dir ──────────────────────────────────
TMP="$(mktemp -d /tmp/e2e-team.XXXXXX)"
DB="$TMP/hub.db"
BODY="$TMP/body"
HUB_PID=""
teardown() {
  if [ "${KEEP:-}" = "1" ]; then
    printf '\nKEEP=1: hub pid %s still running, state in %s\n' "${HUB_PID:-?}" "$TMP"
    return
  fi
  [ -n "$HUB_PID" ] && kill "$HUB_PID" 2>/dev/null || true
  rm -rf "$TMP"
}
trap 'teardown' EXIT

# Stripe must be ABSENT: the point of this harness is that Team works without
# it (seats via the admin CLI, billing:false). Also drop any inherited hub env.
# The CCHUB_SMTP_URL / CCHUB_MAIL_* scrub is the same discipline applied to the
# 0073 mailer: a stray relay credential in the operator's shell must never turn
# this harness into something that sends real mail. CCHUB_MAIL_DIR is then set
# BELOW, deliberately, and wins over CCHUB_SMTP_URL by construction (mailer.rs).
unset STRIPE_SECRET_KEY STRIPE_WEBHOOK_SECRET \
      STRIPE_PRICE_PRO_MONTHLY STRIPE_PRICE_PRO_ANNUAL STRIPE_PRICE_PRO_FOUNDER \
      STRIPE_PRICE_TEAM_MONTHLY STRIPE_PRICE_TEAM_ANNUAL STRIPE_PRICE_TEAM_FOUNDER \
      STRIPE_FOUNDER_DEADLINE STRIPE_MANAGED_PAYMENTS \
      ANTHROPIC_API_KEY CCHUB_SUPPORT_EMAIL CCHUB_OAUTH_ONLY \
      GOOGLE_OAUTH_CLIENT_ID GOOGLE_OAUTH_CLIENT_SECRET \
      CCHUB_SMTP_URL CCHUB_MAIL_DIR CCHUB_MAIL_FROM CCHUB_MAIL_REPLY_TO \
      CCHUB_MAIL_FOOTER \
      CCWEB_PASSWORD CCWEB_API_TOKEN CCHUB_AGENT_TOKENS 2>/dev/null || true
export CCHUB_DATABASE_URL="sqlite://$DB"
export CCWEB_CONFIG_DIR="$TMP/hub-config"
export CCHUB_PUBLIC_URL="$HUB"
# 0073's capture transport: every message is written here as an .eml instead of
# being sent. Together with CCHUB_PUBLIC_URL (which the mailer REQUIRES — an
# unset base disables it entirely) this is the whole mail configuration.
MAILDIR="$TMP/mail"
export CCHUB_MAIL_DIR="$MAILDIR"

say "0/15: start a local multi-tenant hub on $HUB (db $DB)"
"$HUB_BIN" --addr "127.0.0.1:$PORT" >"$TMP/hub.log" 2>&1 &
HUB_PID=$!
ready=""
for _ in $(seq 1 60); do
  if curl -fsS "$HUB/api/me" >/dev/null 2>&1; then ready=1; break; fi
  kill -0 "$HUB_PID" 2>/dev/null || die "hub exited early — $(tail -5 "$TMP/hub.log")"
  sleep 0.5
done
[ -n "$ready" ] || die "hub never answered /api/me on $HUB — $(tail -5 "$TMP/hub.log")"
curl -fsS "$HUB/api/me" | jq -e '.multiTenant == true and .billing == false' >/dev/null \
  || die "hub is not multi-tenant (or Stripe leaked in) — check $TMP/hub.log"
pass "hub up, multiTenant:true, billing:false"

# ── helpers ─────────────────────────────────────────────────────────────────
# req METHOD PATH JAR [JSON] → sets STATUS + RESP.
req() {
  local m="$1" p="$2" jar="$3" data="${4:-}"
  if [ -n "$data" ]; then
    STATUS="$(curl -sS -o "$BODY" -w '%{http_code}' -X "$m" -b "$jar" -c "$jar" \
              -H 'content-type: application/json' -d "$data" "$HUB$p" || true)"
  else
    STATUS="$(curl -sS -o "$BODY" -w '%{http_code}' -X "$m" -b "$jar" -c "$jar" "$HUB$p" || true)"
  fi
  RESP="$(cat "$BODY" 2>/dev/null || true)"
}
# sq SQL → one-line result (busy-tolerant: the hub holds the same file open).
sq() {
  if [ "$SQL_VIA" = "sqlite3" ]; then
    sqlite3 -cmd '.timeout 5000' "$DB" "$1"
  else
    python3 - "$DB" "$1" <<'PYEOF'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1], timeout=5)
con.execute("PRAGMA busy_timeout=5000")
cur = con.executescript(sys.argv[2]) if ";" in sys.argv[2].rstrip(";") else con.execute(sys.argv[2])
try:
    rows = cur.fetchall()
except sqlite3.ProgrammingError:
    rows = []
con.commit()
for r in rows:
    print("|".join(str(c) for c in r))
con.close()
PYEOF
  fi
}
# hubcli … → the operator CLI against the same DB.
hubcli() { "$HUB_BIN" "$@"; }
# expect STEP DESCRIPTION WANT-STATUS → pass/fail on $STATUS.
expect() {
  if [ "$STATUS" = "$3" ]; then pass "$1 $2 → $3"; else badfl "$1 $2 → got $STATUS, want $3 ($RESP)"; fi
}
# body_has STEP DESCRIPTION SUBSTRING → pass/fail on $RESP contents.
body_has() {
  case "$RESP" in
    *"$3"*) pass "$1 $2" ;;
    *) badfl "$1 $2 — body lacks \"$3\": $RESP" ;;
  esac
}

# ── captured-mail helpers (0073 E1) ─────────────────────────────────────────
# The send is SPAWNED, never awaited (org.rs invite_create), so the .eml lands
# shortly AFTER the 200 comes back. Every assertion below therefore waits for a
# count with a bounded timeout instead of reading the directory immediately —
# reading it straight away is the one way to make these checks flaky.
mail_count() { ls -1 "$MAILDIR"/*.eml 2>/dev/null | wc -l | tr -d ' '; }
# wait_mail N → 0 once at least N messages have been captured (~10s ceiling).
wait_mail() {
  local want="$1" i=0
  while [ "$i" -lt 100 ]; do
    if [ "$(mail_count)" -ge "$want" ]; then return 0; fi
    sleep 0.1; i=$((i + 1))
  done
  return 1
}
# mail_to ADDRESS → how many captured messages are addressed to ADDRESS.
mail_to() { grep -lF "To: $1" "$MAILDIR"/*.eml 2>/dev/null | wc -l | tr -d ' '; }
# mail_body_has ADDRESS SUBSTRING → 0 when ADDRESS's message carries SUBSTRING.
# Bodies can be quoted-printable, so soft line breaks (a trailing "=") are
# unfolded first: a 60-odd-character invite URL sits right at the 76-col wrap.
mail_body_has() {
  local f
  for f in $(grep -lF "To: $1" "$MAILDIR"/*.eml 2>/dev/null); do
    if tr -d '\r' <"$f" | awk '{ if (sub(/=$/, "")) printf "%s", $0; else print }' | grep -qF "$2"; then
      return 0
    fi
  done
  return 1
}

TS="$(date +%s)"
PW="e2e-$(openssl rand -hex 12 2>/dev/null || echo "fallback-password-$TS")"
O="e2e-o-$TS@ccscreen.test";  JAR_O="$TMP/jar-o"
A="e2e-a-$TS@ccscreen.test";  JAR_A="$TMP/jar-a"
B="e2e-b-$TS@ccscreen.test";  JAR_B="$TMP/jar-b"
C="e2e-c-$TS@ccscreen.test";  JAR_C="$TMP/jar-c"
D="e2e-d-$TS@ccscreen.test";  JAR_D="$TMP/jar-d"
E="e2e-e-$TS@ccscreen.test"   # never signs up — invite-revoke target
M1="e2e-m1-$TS"; M2="e2e-m2-$TS"; M3="e2e-m3-$TS"; M4="e2e-m4-$TS"
ORG_NAME="e2e-team-$TS"

signup() { # EMAIL JAR
  req POST /api/signup "$2" "{\"email\":\"$1\",\"password\":\"$PW\"}"
  [ "$STATUS" = "200" ] || die "signup $1 answered $STATUS: $RESP"
}

# ─────────────────────────────────────────────────────────────────────────────
say "1/15: owner + member-A sign up; no org yet → /api/orgs/mine 404"
signup "$O" "$JAR_O"; signup "$A" "$JAR_A"
pass "signed up $O and $A"
req GET /api/orgs/mine "$JAR_O"
expect "1" "orgs/mine before any org" 404

# ─────────────────────────────────────────────────────────────────────────────
say "2/15: owner creates the org (dormant, 0 seats); a second create → 409"
req POST /api/orgs "$JAR_O" "{\"name\":\"$ORG_NAME\"}"
expect "2" "POST /api/orgs" 200
ORG="$(printf '%s' "$RESP" | jq -r '.id // empty')"
[ -n "$ORG" ] || die "org create returned no id: $RESP"
req GET /api/orgs/mine "$JAR_O"
if printf '%s' "$RESP" | jq -e --arg n "$ORG_NAME" \
     '.myRole == "owner" and .org.name == $n and .org.seats == 0 and .org.memberCount == 1' >/dev/null; then
  pass "2 orgs/mine: owner of \"$ORG_NAME\", 0 seats, 1 member"
else
  badfl "2 orgs/mine shape wrong: $RESP"
fi
[ "$(sq "SELECT COUNT(*) FROM audit_log WHERE org_id='$ORG' AND action='org.created';")" = "1" ] \
  && pass "2 SQL: org.created audit row" || badfl "2 SQL: no org.created audit row"
req POST /api/orgs "$JAR_O" '{"name":"second-org"}'
expect "2" "second org create (one-org rule)" 409

# ─────────────────────────────────────────────────────────────────────────────
say "3/15: enroll machines pre-join (admin CLI): $M1 → owner, $M2 → A"
hubcli user agent "$O" "$M1" >/dev/null || die "user agent $O $M1 failed"
hubcli user agent "$A" "$M2" >/dev/null || die "user agent $A $M2 failed"
pass "enrolled $M1 (owner) and $M2 (A)"

# ─────────────────────────────────────────────────────────────────────────────
say "4/15: invite A (existing account) and B (no account) — uniform responses + uniform mail"
[ "$(mail_count)" = "0" ] \
  && pass "4 no mail captured before the first invite" \
  || badfl "4 maildir is not empty before the first invite ($(mail_count) files)"
req POST /api/orgs/invites "$JAR_O" "{\"email\":\"$A\"}"
expect "4" "invite A" 200
INV_A_KEYS="$(printf '%s' "$RESP" | jq -cS 'keys')"
req POST /api/orgs/invites "$JAR_O" "{\"email\":\"$B\"}"
expect "4" "invite B" 200
INV_B_KEYS="$(printf '%s' "$RESP" | jq -cS 'keys')"
INV_B_URL="$(printf '%s' "$RESP" | jq -r '.invite_url // empty')"
INV_B_STATUS="$(printf '%s' "$RESP" | jq -r '.status // empty')"
if [ "$INV_A_KEYS" = "$INV_B_KEYS" ] && [ -n "$INV_B_URL" ] && [ "$INV_B_STATUS" = "pending" ]; then
  pass "4 both arms answer the same shape $INV_A_KEYS (no account-existence oracle)"
else
  badfl "4 invite arms differ: A=$INV_A_KEYS B=$INV_B_KEYS status=$INV_B_STATUS url=$INV_B_URL"
fi
B_TOKEN="${INV_B_URL##*/}"
# The mail twin of the shape assertion above (0073 E1). The create response
# deliberately carries no `emailed` field, so the only way the mailer could
# become the account-existence oracle this step exists to catch is by sending in
# one arm and not the other — assert EQUAL counts, and that B (the arm that used
# to get nothing at all) received a message carrying B's own invite token.
if wait_mail 2; then
  MAIL_A="$(mail_to "$A")"; MAIL_B="$(mail_to "$B")"
  if [ "$MAIL_A" = "$MAIL_B" ] && [ "$MAIL_A" = "1" ]; then
    pass "4 both arms mailed exactly once (A=$MAIL_A, B=$MAIL_B — no delivery-side oracle)"
  else
    badfl "4 invite arms mailed differently: A=$MAIL_A B=$MAIL_B (captures: $(mail_count))"
  fi
  [ "$(mail_count)" = "2" ] \
    && pass "4 exactly 2 messages captured for 2 invites" \
    || badfl "4 expected 2 captured messages, found $(mail_count)"
  mail_body_has "$B" "$B_TOKEN" \
    && pass "4 B's message carries B's invite token" \
    || badfl "4 B's captured message does not carry B's token ($B_TOKEN)"
  mail_body_has "$B" "$INV_B_URL" \
    && pass "4 B's message carries the absolute invite link $INV_B_URL" \
    || badfl "4 B's captured message lacks the absolute invite URL"
else
  badfl "4 only $(mail_count) message(s) captured in $MAILDIR within 10s (want 2)"
fi

# ─────────────────────────────────────────────────────────────────────────────
say "5/15: zero-seat gate — A's accept on the dormant org → 402 'no seats yet'"
req GET /api/orgs/invites/inbox "$JAR_A"
INV_A_ID="$(printf '%s' "$RESP" | jq -r '.[0].id // empty')"
[ -n "$INV_A_ID" ] || die "A's inbox is empty: $RESP"
printf '%s' "$RESP" | jq -e --arg n "$ORG_NAME" --arg o "$O" \
    '.[0].orgName == $n and .[0].inviterEmail == $o and (.[0].consent | length > 0)' >/dev/null \
  && pass "5 inbox row carries orgName + inviterEmail + consent" \
  || badfl "5 inbox row incomplete: $RESP"
req POST "/api/orgs/invites/$INV_A_ID/accept" "$JAR_A"
expect "5" "accept with 0 seats" 402
body_has "5" "402 names the missing seats" "no seats yet"

# ─────────────────────────────────────────────────────────────────────────────
say "6/15: 'cc-screen-hub org seats' activates 3 seats (CLI + SQL + audit)"
out="$(hubcli org seats "$ORG" 3)" || die "org seats failed: $out"
case "$out" in *"3 seats"*) pass "6 CLI: $out" ;; *) badfl "6 CLI output unexpected: $out" ;; esac
[ "$(sq "SELECT seat_count FROM orgs WHERE id='$ORG';")" = "3" ] \
  && pass "6 SQL: seat_count=3" || badfl "6 SQL: seat_count != 3"
hubcli org info "$ORG" | grep -q "seats: 3" \
  && pass "6 CLI: org info reports 3 seats" || badfl "6 CLI: org info lacks 'seats: 3'"
[ "$(sq "SELECT COUNT(*) FROM audit_log WHERE org_id='$ORG' AND action='org.seats_changed';")" = "1" ] \
  && pass "6 SQL: org.seats_changed audit row" || badfl "6 SQL: no org.seats_changed audit row"

# ─────────────────────────────────────────────────────────────────────────────
say "7/15: A accepts → member row + member.joined audit row"
req POST "/api/orgs/invites/$INV_A_ID/accept" "$JAR_A"
expect "7" "A accept" 200
body_has "7" "accept answers accepted" '"accepted"'
[ "$(sq "SELECT COUNT(*) FROM org_members WHERE org_id='$ORG';")" = "2" ] \
  && pass "7 SQL: 2 member rows" || badfl "7 SQL: member count wrong"
[ "$(sq "SELECT COUNT(*) FROM audit_log WHERE org_id='$ORG' AND action='member.joined';")" = "1" ] \
  && pass "7 SQL: member.joined audit row" || badfl "7 SQL: no member.joined audit row"
req GET /api/orgs/mine "$JAR_A"
printf '%s' "$RESP" | jq -e '.myRole == "member"' >/dev/null \
  && pass "7 A is a member" || badfl "7 A's role wrong: $RESP"

# ─────────────────────────────────────────────────────────────────────────────
say "8/15: B — public invite-link read (verbatim consent), signup, inbox accept"
# The landing read is unauthenticated: the token IS the capability.
req GET "/api/org-invite/$B_TOKEN" "$TMP/jar-anon"
expect "8" "GET /api/org-invite/:token (public)" 200
CONSENT="Joining makes your machines on cc-screen visible to this team (view-only). You can hide any machine in settings."
if printf '%s' "$RESP" | jq -e --arg c "$CONSENT" --arg b "$B" --arg n "$ORG_NAME" --arg o "$O" \
     '.kind == "team" and .email == $b and .orgName == $n and .inviterEmail == $o and .consent == $c' >/dev/null; then
  pass "8 landing payload: kind/team, addressee, orgName, inviter, VERBATIM consent copy"
else
  badfl "8 landing payload wrong (consent copy drifted?): $RESP"
fi
signup "$B" "$JAR_B"
req GET /api/orgs/invites/inbox "$JAR_B"
INV_B_ID="$(printf '%s' "$RESP" | jq -r '.[0].id // empty')"
[ -n "$INV_B_ID" ] || die "B's inbox is empty after signup: $RESP"
req POST "/api/orgs/invites/$INV_B_ID/accept" "$JAR_B"
expect "8" "B accept" 200
[ "$(sq "SELECT COUNT(*) FROM org_members WHERE org_id='$ORG';")" = "3" ] \
  && pass "8 SQL: 3 member rows" || badfl "8 SQL: member count wrong"

# ─────────────────────────────────────────────────────────────────────────────
say "9/15: seat gate + one-org — C's accept 402s, then 409s from another team"
req POST /api/orgs/invites "$JAR_O" "{\"email\":\"$C\"}"
expect "9" "invite C" 200
signup "$C" "$JAR_C"
req GET /api/orgs/invites/inbox "$JAR_C"
INV_C_ID="$(printf '%s' "$RESP" | jq -r '.[0].id // empty')"
req POST "/api/orgs/invites/$INV_C_ID/accept" "$JAR_C"
expect "9" "C accept on a full org" 402
body_has "9" "402 names the seat count" "out of seats (3)"
# C founds their own team → the earlier invite now hits the one-org rule.
req POST /api/orgs "$JAR_C" "{\"name\":\"e2e-team-c-$TS\"}"
expect "9" "C creates their own org" 200
CORG="$(printf '%s' "$RESP" | jq -r '.id // empty')"
req POST "/api/orgs/invites/$INV_C_ID/accept" "$JAR_C"
expect "9" "C accept while in another team" 409

# ─────────────────────────────────────────────────────────────────────────────
say "10/15: pooled limits in /api/me — plan team, 10×seats / 50×seats"
for who in "$JAR_O:owner" "$JAR_A:member-A"; do
  jar="${who%%:*}"; label="${who##*:}"
  req GET /api/me "$jar"
  if printf '%s' "$RESP" | jq -e --arg org "$ORG" \
       '.plan.name == "team" and .plan.maxAgents == 30 and .plan.maxSessions == 150
        and .plan.seats == 3 and .plan.members == 3 and .plan.orgId == $org' >/dev/null; then
    pass "10 $label: plan=team 30/150, seats=3, members=3, orgId"
  else
    badfl "10 $label plan block wrong: $(printf '%s' "$RESP" | jq -c '.plan')"
  fi
done

# ─────────────────────────────────────────────────────────────────────────────
say "11/15: pooled machine gate — tune team row to 1/seat, device-approve 402s"
# Hand-tune the per-seat contribution so the pool cap is reachable (3 = 1×3).
sq "UPDATE plan_limits SET max_agents=1 WHERE plan='team';"
req GET /api/me "$JAR_O"
printf '%s' "$RESP" | jq -e '.plan.maxAgents == 3' >/dev/null \
  && pass "11 tuned pool cap visible in /api/me (maxAgents=3)" \
  || badfl "11 tuned maxAgents wrong: $(printf '%s' "$RESP" | jq -c '.plan')"
# Fill the pool: the CLI enroll bypasses the gate by design (operator tooling).
hubcli user agent "$A" "$M3" >/dev/null || die "user agent $A $M3 failed"
# A NEW label over the pool cap → the device flow's approve answers 402.
req POST /api/device/code "$TMP/jar-anon" "{\"device_id\":\"e2e-dev-$TS\",\"machine_id\":\"$M4\"}"
UC="$(printf '%s' "$RESP" | jq -r '.user_code // empty')"
[ -n "$UC" ] || die "device code mint failed: $RESP"
req POST /api/device/approve "$JAR_B" "{\"user_code\":\"$UC\"}"
expect "11" "approve a NEW machine over the pool" 402
body_has "11" "402 is the pool message" "Team machine pool full (3 across 3 seats)"
# A re-enroll of an EXISTING label is exempt (rotates the token, adds nothing).
req POST /api/device/code "$TMP/jar-anon" "{\"device_id\":\"e2e-dev2-$TS\",\"machine_id\":\"$M2\"}"
UC="$(printf '%s' "$RESP" | jq -r '.user_code // empty')"
req POST /api/device/approve "$JAR_A" "{\"user_code\":\"$UC\"}"
expect "11" "re-enroll of an existing label stays exempt" 200
# (That approve also re-runs the materializer, covering the CLI-enrolled $M3.)
sq "UPDATE plan_limits SET max_agents=10 WHERE plan='team';"
req GET /api/me "$JAR_O"
printf '%s' "$RESP" | jq -e '.plan.maxAgents == 30' >/dev/null \
  && pass "11 pool cap restored (maxAgents=30)" || badfl "11 restore failed"

# ─────────────────────────────────────────────────────────────────────────────
say "12/15: team visibility — B sees A's machine view-only; leave/revoke 409"
req GET /api/orgs/mine "$JAR_A"
AG2="$(printf '%s' "$RESP" | jq -r --arg m "$M2" '.machines[] | select(.machine == $m) | .agentId')"
[ -n "$AG2" ] || die "A's orgs/mine lists no agentId for $M2: $RESP"
req GET /api/shares/received "$JAR_B"
ROW2="$(printf '%s' "$RESP" | jq -c --arg id "$AG2" '.[] | select(.agentId == $id)')"
if [ -n "$ROW2" ] && printf '%s' "$ROW2" | jq -e --arg n "$ORG_NAME" \
     '.kind == "team" and .origin == "team" and .permission == "view" and .orgName == $n' >/dev/null; then
  pass "12 B's received list: A's machine as kind=team origin=team permission=view orgName"
else
  badfl "12 B's team row for $M2 wrong/missing: $ROW2 (full: $RESP)"
fi
TEAM_ROWS_B="$(req GET /api/shares/received "$JAR_B"; printf '%s' "$RESP" | jq '[.[] | select(.origin == "team")] | length')"
[ "${TEAM_ROWS_B:-0}" -ge 2 ] 2>/dev/null \
  && pass "12 B holds $TEAM_ROWS_B team rows (owner's + A's machines)" \
  || badfl "12 expected >=2 team rows for B, got $TEAM_ROWS_B"
SHARE2_ID="$(printf '%s' "$ROW2" | jq -r '.id // empty')"
req POST "/api/shares/received/$SHARE2_ID/leave" "$JAR_B"
expect "12" "leave on a team row" 409
body_has "12" "409 explains the team management" "managed by your team"
req POST "/api/shares/$SHARE2_ID/revoke" "$JAR_B"
expect "12" "revoke on a team row" 409
# View-only: B cannot create a session on A's machine. (No live agent is
# attached in this harness, so 404 is also the offline answer — the authoritative
# view-vs-use predicate tests live in crates/hub's registry/db unit suites.)
req POST "/api/session?machine=$M2" "$JAR_B" '{"tool":"shell","name":"e2e-x","dir":"~"}'
expect "12" "B session-create on A's machine" 404

# ─────────────────────────────────────────────────────────────────────────────
say "13/15: the owner opt-out toggle — audited, immediate on the received list"
req POST "/api/orgs/machines/$AG2/visibility" "$JAR_A" '{"visible":false}'
expect "13" "A hides $M2 from the team" 204
req GET /api/shares/received "$JAR_B"
[ "$(printf '%s' "$RESP" | jq --arg id "$AG2" '[.[] | select(.agentId == $id)] | length')" = "0" ] \
  && pass "13 B's received list lost the hidden machine immediately" \
  || badfl "13 hidden machine still visible to B: $RESP"
[ "$(sq "SELECT COUNT(*) FROM audit_log WHERE org_id='$ORG' AND action='machine.visibility_changed' AND detail LIKE '%$M2%';")" = "1" ] \
  && pass "13 SQL: machine.visibility_changed audit row names the machine" \
  || badfl "13 SQL: visibility audit row missing/unlabelled"
req POST "/api/orgs/machines/$AG2/visibility" "$JAR_A" '{"visible":true}'
expect "13" "A re-shows $M2" 204
req GET /api/shares/received "$JAR_B"
[ "$(printf '%s' "$RESP" | jq --arg id "$AG2" '[.[] | select(.agentId == $id)] | length')" = "1" ] \
  && pass "13 re-shown machine is back in B's received list" \
  || badfl "13 re-shown machine did not come back: $RESP"
# B (not the owner of the machine) must NOT be able to toggle it.
req POST "/api/orgs/machines/$AG2/visibility" "$JAR_B" '{"visible":false}'
expect "13" "B toggling A's machine (owner-only)" 404

# ─────────────────────────────────────────────────────────────────────────────
say "14/15: role & permission matrix"
UID_A="$(req GET /api/me "$JAR_A"; printf '%s' "$RESP" | jq -r '.userId')"
UID_O="$(req GET /api/me "$JAR_O"; printf '%s' "$RESP" | jq -r '.userId')"
UID_B="$(req GET /api/me "$JAR_B"; printf '%s' "$RESP" | jq -r '.userId')"
req POST /api/orgs/invites "$JAR_B" "{\"email\":\"$D\"}"
expect "14" "member B invites" 403
req GET /api/orgs/invites "$JAR_B"
expect "14" "member B reads the outbox" 403
req GET /api/orgs/audit "$JAR_B"
expect "14" "member B reads the audit log" 404
signup "$D" "$JAR_D"
req GET /api/orgs/mine "$JAR_D"
expect "14" "non-member orgs/mine" 404
req POST "/api/orgs/members/$UID_A/role" "$JAR_B" '{"role":"admin"}'
expect "14" "member B changes a role" 403
req POST /api/orgs/leave "$JAR_O"
expect "14" "owner leave without transfer" 409
req POST "/api/orgs/members/$UID_A/role" "$JAR_O" '{"role":"admin"}'
expect "14" "owner promotes A to admin" 204
req GET /api/orgs/invites "$JAR_A"
expect "14" "admin A reads the outbox" 200
req POST "/api/orgs/members/$UID_O/remove" "$JAR_A"
expect "14" "admin A removes the owner" 403
# Invite lifecycle extras: decline (D) and revoke (E, never signs up).
req POST /api/orgs/invites "$JAR_O" "{\"email\":\"$D\"}"
INV_D_ID="$(printf '%s' "$RESP" | jq -r '.id // empty')"
req GET /api/orgs/invites/inbox "$JAR_D"
req POST "/api/orgs/invites/$INV_D_ID/decline" "$JAR_D"
expect "14" "D declines" 200
body_has "14" "decline answers declined" '"declined"'
req POST /api/orgs/invites "$JAR_O" "{\"email\":\"$E\"}"
INV_E_ID="$(printf '%s' "$RESP" | jq -r '.id // empty')"
req POST "/api/orgs/invites/$INV_E_ID/revoke" "$JAR_O"
expect "14" "owner revokes E's pending invite" 200
body_has "14" "revoke answers revoked" '"revoked"'
# Ownership transfer (atomic: exactly one owner), then removal + leave.
req POST "/api/orgs/members/$UID_A/role" "$JAR_O" '{"role":"owner"}'
expect "14" "ownership transfer O→A" 204
req GET /api/orgs/mine "$JAR_O"
printf '%s' "$RESP" | jq -e '.myRole == "admin"' >/dev/null \
  && pass "14 old owner demoted to admin" || badfl "14 old owner's role wrong: $RESP"
req GET /api/orgs/mine "$JAR_A"
printf '%s' "$RESP" | jq -e '.myRole == "owner"' >/dev/null \
  && pass "14 A is the owner now" || badfl "14 A's role wrong: $RESP"
req POST "/api/orgs/members/$UID_B/remove" "$JAR_A"
expect "14" "owner A removes member B" 204
req GET /api/orgs/mine "$JAR_B"
expect "14" "removed B's orgs/mine" 404
[ "$(sq "SELECT COUNT(*) FROM shares WHERE grantee_user_id='$UID_B' AND kind='team';")" = "0" ] \
  && pass "14 SQL: B's team-share rows pruned on removal" \
  || badfl "14 SQL: B still holds team-share rows after removal"
req POST /api/orgs/leave "$JAR_O"
expect "14" "admin O leaves" 204
[ "$(sq "SELECT COUNT(*) FROM org_members WHERE org_id='$ORG';")" = "1" ] \
  && pass "14 SQL: 1 member remains (A, the owner)" || badfl "14 SQL: member count wrong"

# ─────────────────────────────────────────────────────────────────────────────
say "15/15: the audit log — full vocabulary present, paging strictly descending"
req GET "/api/orgs/audit?limit=100" "$JAR_A"
expect "15" "owner reads the audit log" 200
# invite.emailed is 0073's — appended by the spawned send task, actor NULL (the
# mailer, not a human). It is in the fixed list because a vocabulary this loop
# does not name is a vocabulary nothing checks.
for action in org.created org.seats_changed invite.created invite.emailed \
              member.joined invite.declined invite.revoked member.role_changed \
              machine.visibility_changed member.removed member.left; do
  n="$(printf '%s' "$RESP" | jq --arg a "$action" '[.[] | select(.action == $a)] | length')"
  [ "${n:-0}" -ge 1 ] 2>/dev/null \
    && pass "15 audit has $action (×$n)" || badfl "15 audit lacks $action"
done
req GET "/api/orgs/audit?limit=5" "$JAR_A"
P1_IDS="$(printf '%s' "$RESP" | jq -c '[.[].id]')"
P1_DESC="$(printf '%s' "$P1_IDS" | jq '. == (. | sort | reverse) and length == 5')"
CURSOR="$(printf '%s' "$P1_IDS" | jq 'min')"
req GET "/api/orgs/audit?limit=5&before=$CURSOR" "$JAR_A"
P2_IDS="$(printf '%s' "$RESP" | jq -c '[.[].id]')"
P2_OK="$(printf '%s' "$P2_IDS" | jq --argjson c "$CURSOR" '. == (. | sort | reverse) and (length > 0) and (max < $c)')"
if [ "$P1_DESC" = "true" ] && [ "$P2_OK" = "true" ]; then
  pass "15 paging: page1 $P1_IDS strictly descending; page2 $P2_IDS all older than $CURSOR"
else
  badfl "15 paging broken: page1=$P1_IDS page2=$P2_IDS cursor=$CURSOR"
fi

# ─────────────────────────────────────────────────────────────────────────────
say "teardown: DELETE the org — CASCADE must reap members/invites/audit/team-shares"
# sqlite3 defaults foreign_keys OFF, so the pragma is load-bearing here.
sq "PRAGMA foreign_keys=ON; DELETE FROM orgs WHERE id IN ('$ORG','$CORG');"
for probe in \
  "org_members:SELECT COUNT(*) FROM org_members WHERE org_id='$ORG';" \
  "org_invites:SELECT COUNT(*) FROM org_invites WHERE org_id='$ORG';" \
  "audit_log:SELECT COUNT(*) FROM audit_log WHERE org_id='$ORG';" \
  "team shares:SELECT COUNT(*) FROM shares WHERE org_id='$ORG';"; do
  label="${probe%%:*}"; query="${probe#*:}"
  n="$(sq "$query")"
  [ "$n" = "0" ] && pass "teardown CASCADE: $label reaped" \
                 || badfl "teardown CASCADE: $n $label rows survived org deletion"
done
# Machines are NEVER deleted by org lifecycle (visibility, not ownership).
n="$(sq "SELECT COUNT(*) FROM agents;")"
[ "$n" = "3" ] && pass "teardown: all 3 machine rows intact (orgs never own machines)" \
              || badfl "teardown: expected 3 agents rows, found $n"

# ─────────────────────────────────────────────────────────────────────────────
echo
if [ "$rc" -eq 0 ]; then
  printf '%sTEAM E2E OK%s (local hub on %s)\n' "$C_G" "$C_0" "$HUB"
else
  printf '%sTEAM E2E FAILED%s (local hub on %s — log: %s)\n' "$C_R" "$C_0" "$HUB" "$TMP/hub.log"
  [ "${KEEP:-}" = "1" ] || printf 'Re-run with KEEP=1 to keep the hub + db for inspection.\n'
fi
exit "$rc"
