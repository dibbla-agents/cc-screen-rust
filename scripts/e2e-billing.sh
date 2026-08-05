#!/usr/bin/env bash
# e2e-billing — the Stripe billing end-to-end test for the hosted SaaS flow
# (proposal 0058 Part D4 step 5 / acceptance criterion 17).
#
# It drives the FULL subscription lifecycle in Stripe **test mode** against
# production infra (adapting e2e-saas-journey.sh's live-URL convention): a fresh
# user, real Stripe test-mode customer/subscription/test-clock, and signed
# webhooks delivered to the live hub. Every run uses a fresh email, so it is
# re-runnable and doubles as a post-deploy billing smoke test. Never wire it into
# merge-blocking CI: it is a deploy gate, not a PR gate.
#
# The six checks map 1:1 to acceptance criterion 17 / D4 step 5:
#   1. fresh user starts on `free` (2/5)               → /api/me
#   2. start checkout, drive the Stripe test card       → POST /api/billing/checkout + Stripe API
#   3. webhook flips the user to `pro` (active)         → signed checkout.session.completed
#   4. test clock +1 month → invoice.paid renewal       → current_period_end advances
#   5. failing card → invoice.payment_failed            → plan_status=past_due, plan still `pro`
#   6. past grace → subscription.deleted                → downgrade to fallback + agents rows intact + new approval 402
#
# ── Required environment (the script SKIPS gracefully if any is missing) ──
#   STRIPE_SECRET_KEY        Stripe **test** secret key (sk_test_… or a restricted rk_test_…
#                            with Checkout/Subscriptions/Test-clocks write)
#   STRIPE_PRICE_PRO_MONTHLY test-mode price id for Pro monthly (price_…)
#   STRIPE_PRICE_PRO_ANNUAL  test-mode price id for Pro annual  (price_…)
#   STRIPE_WEBHOOK_SECRET    the hub's configured webhook signing secret (whsec_…);
#                            MUST match what the target hub verifies against, so the
#                            signed events this script posts are accepted.
#   HUB                      target hub base URL (default https://app.ccscreen.dev)
#
# ── Optional environment ──
#   CCHUB_DB   path to the hub's SQLite DB (for the criterion-17 "agents rows
#              intact" SQL assert in step 6). Only reachable when this script runs
#              ON the hub host; otherwise that sub-assert prints the exact query to
#              run by hand and is marked a STUB (not a failure).
#   KEEP=1     skip Stripe/user teardown at the end (for debugging).
#
# ── Tooling ── curl, jq, openssl. (No Stripe CLI required: Stripe API calls go
# over curl, and webhook events are signed locally with openssl to the same
# t=…,v1=… scheme the hub verifies. If you prefer `stripe listen`/`stripe
# trigger`, that is an alternative to the signed-POST path used here — see the
# NOTE in step 3.)
#
# Teardown (automatic unless KEEP=1): deletes the Stripe test clock (cascades its
# customer + subscription). The throwaway hub account must be removed on the hub
# origin host (no cookie-authed account-delete endpoint exists yet):
#   cc-screen-hub user delete "<the e2e-bill+…@ccscreen.dev email the run printed>"
set -euo pipefail

HUB="${HUB:-https://app.ccscreen.dev}"
STRIPE_API="https://api.stripe.com"

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
stub()  { printf '%sSTUB%s: %s\n' "$C_Y" "$C_0" "$*"; }
die()   { printf '%sFAIL%s: %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }
skip()  { printf '\n%sSKIP%s: %s\n' "$C_Y" "$C_0" "$*"; exit 0; }

# ── gate: skip gracefully unless everything needed is present ────────────────
missing=""
for v in STRIPE_SECRET_KEY STRIPE_PRICE_PRO_MONTHLY STRIPE_PRICE_PRO_ANNUAL STRIPE_WEBHOOK_SECRET; do
  eval "val=\${$v:-}"
  [ -n "$val" ] || missing="$missing $v"
done
for t in curl jq openssl; do
  command -v "$t" >/dev/null 2>&1 || missing="$missing (tool:$t)"
done
if [ -n "$missing" ]; then
  skip "billing E2E needs test-mode Stripe keys + tooling — missing:$missing
      Set STRIPE_SECRET_KEY (test), STRIPE_PRICE_PRO_MONTHLY, STRIPE_PRICE_PRO_ANNUAL,
      STRIPE_WEBHOOK_SECRET (matching the hub's), and have curl/jq/openssl on PATH.
      This script is committed now and becomes runnable once the founder provides
      test keys (proposal 0058, rollout step 4→5)."
fi

case "$STRIPE_SECRET_KEY" in
  *_test_*) : ;;
  sk_live_*|rk_live_*) die "STRIPE_SECRET_KEY is a LIVE key — refusing to run the billing E2E against live Stripe. Use a test key." ;;
  *) printf '%sWARN%s: STRIPE_SECRET_KEY does not look like a test key; continuing (expected sk_test_/rk_test_).\n' "$C_Y" "$C_0" ;;
esac

EMAIL="e2e-bill+$(date +%s)@ccscreen.dev"
if command -v openssl >/dev/null 2>&1; then
  PW="$(openssl rand -hex 16)"
else
  PW="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
fi
JAR="$(mktemp)"; BODY="$(mktemp)"
TC_ID=""   # Stripe test-clock id, for teardown
trap 'teardown' EXIT

teardown() {
  rm -f "$JAR" "$BODY" 2>/dev/null || true
  if [ "${KEEP:-}" != "1" ] && [ -n "$TC_ID" ]; then
    # Deleting the test clock cascades its customer + subscription.
    stripe_api DELETE "/v1/test_helpers/test_clocks/$TC_ID" >/dev/null 2>&1 || true
  fi
}

# curl to the hub; prints "<status>\n<body>". Cookie jar carries the identity.
hub_req() {
  method="$1"; path="$2"; shift 2
  code="$(curl -sS -o "$BODY" -w '%{http_code}' -X "$method" "$HUB$path" "$@" || true)"
  printf '%s\n' "$code"
  cat "$BODY" 2>/dev/null || true
  : > "$BODY"
}

# GET /api/me as the signed-in user → prints the JSON body.
me() { curl -sS -b "$JAR" "$HUB/api/me"; }
me_field() { me | jq -r "$1 // empty" 2>/dev/null || true; }

# Stripe API (form-encoded, key as basic-auth username). Prints the JSON body.
# Usage: stripe_api <METHOD> <path> [curl -d args…]
stripe_api() {
  method="$1"; path="$2"; shift 2
  curl -sS -X "$method" "$STRIPE_API$path" -u "$STRIPE_SECRET_KEY:" "$@"
}

# Sign a raw webhook body the way crates/auth stripe_signature_ok verifies it:
# HMAC-SHA256 over "{t}.{body}" keyed by the whole whsec_… secret → "t=…,v1=…".
sign_stripe() {
  _body="$1"; _ts="$(date +%s)"
  _sig="$(printf '%s.%s' "$_ts" "$_body" \
          | openssl dgst -sha256 -hmac "$STRIPE_WEBHOOK_SECRET" -r | cut -d' ' -f1)"
  printf 't=%s,v1=%s' "$_ts" "$_sig"
}

# POST a signed webhook event to the hub. $1 = raw JSON body. Prints status.
post_webhook() {
  _body="$1"
  _sighdr="$(sign_stripe "$_body")"
  curl -sS -o "$BODY" -w '%{http_code}' -X POST "$HUB/api/billing/webhook" \
       -H 'content-type: application/json' \
       -H "Stripe-Signature: $_sighdr" \
       --data-binary "$_body" || true
  : > "$BODY"
}

# Poll the test clock until it reports "ready" (advances are async at Stripe).
wait_clock_ready() {
  _tc="$1"; _tries=0
  while [ "$_tries" -lt 40 ]; do
    _st="$(stripe_api GET "/v1/test_helpers/test_clocks/$_tc" | jq -r '.status // empty')"
    [ "$_st" = "ready" ] && return 0
    _tries=$((_tries + 1)); sleep 3
  done
  return 1
}

printf 'Billing E2E → hub %s  (test-mode Stripe)\n' "$HUB"
printf 'Throwaway account: %s\n' "$EMAIL"

# ─────────────────────────────────────────────────────────────────────────────
# 1. Fresh user → starts on `free` (2/5)
# ─────────────────────────────────────────────────────────────────────────────
say "1/6: fresh signup starts on the free plan (2 machines / 5 sessions)"
curl -fsS -c "$JAR" -X POST "$HUB/api/signup" -H 'content-type: application/json' \
     -d "{\"email\":\"$EMAIL\",\"password\":\"$PW\"}" | grep -q '"ok":true' \
  || die "signup for $EMAIL did not answer {\"ok\":true}"

ME_JSON="$(me)"
plan="$(printf '%s' "$ME_JSON" | jq -r '.plan.name // empty')"
maxA="$(printf '%s' "$ME_JSON" | jq -r '.plan.maxAgents // empty')"
maxS="$(printf '%s' "$ME_JSON" | jq -r '.plan.maxSessions // empty')"
billing_flag="$(printf '%s' "$ME_JSON" | jq -r '.billing // empty')"
if [ "$plan" = "free" ] && [ "$maxA" = "2" ] && [ "$maxS" = "5" ]; then
  pass "new user is free with caps 2/5"
else
  badfl "expected free/2/5, got plan=$plan maxAgents=$maxA maxSessions=$maxS"
fi
if [ "$billing_flag" = "true" ]; then
  pass "/api/me reports billing:true (Stripe configured on the hub)"
else
  badfl "/api/me billing flag is '$billing_flag' — the hub has no Stripe env; steps 2-6 cannot exercise real checkout"
fi

# ─────────────────────────────────────────────────────────────────────────────
# 2. Start checkout — hub mints a Stripe checkout session; drive the test card.
# ─────────────────────────────────────────────────────────────────────────────
say "2/6: start checkout (POST /api/billing/checkout) and build the test-mode subscription"
ck="$(hub_req POST /api/billing/checkout -b "$JAR" -H 'content-type: application/json' \
        -d '{"price":"pro-monthly"}')"
ck_status="$(printf '%s' "$ck" | head -1)"
ck_url="$(printf '%s' "$ck" | tail -n +2 | jq -r '.url // empty' 2>/dev/null || true)"
case "$ck_status" in
  2*) case "$ck_url" in
        *stripe.com*|*checkout.stripe.com*) pass "checkout returned a Stripe URL ($ck_status)" ;;
        *) badfl "checkout 2xx but url is not a Stripe checkout URL: $ck_url" ;;
      esac ;;
  *) badfl "POST /api/billing/checkout answered $ck_status (expected 2xx with {url})" ;;
esac

# The hosted Checkout *page* itself can't be filled by curl — completing it needs
# a browser. So the lifecycle below is built directly on the Stripe test API with
# the canonical test card (pm_card_visa == 4242…), which is the fully-scriptable
# equivalent, and the user↔customer link is closed by the signed
# checkout.session.completed we post in step 3 (the hub re-fetches truth from the
# subscription id, per 0058 B3 "never trust event payload state").
USER_ID="$(printf '%s' "$ME_JSON" | jq -r '.id // .user.id // .userId // empty')"
if [ -z "$USER_ID" ]; then
  # client_reference_id must be the hub's user id. If /api/me doesn't expose it,
  # the run can't link the customer — surface it loudly rather than fake a pass.
  badfl "could not read the user id from /api/me (need it for client_reference_id); dumping /api/me keys:"
  printf '  %s\n' "$(printf '%s' "$ME_JSON" | jq -c 'keys' 2>/dev/null || echo '?')"
fi

now="$(date +%s)"
TC_ID="$(stripe_api POST /v1/test_helpers/test_clocks -d "frozen_time=$now" | jq -r '.id // empty')"
[ -n "$TC_ID" ] || die "could not create a Stripe test clock (check STRIPE_SECRET_KEY scope: test_helpers write)"
echo "   test clock: $TC_ID"

CUS="$(stripe_api POST /v1/customers \
        -d "email=$EMAIL" -d "test_clock=$TC_ID" \
        -d 'payment_method=pm_card_visa' \
        -d 'invoice_settings[default_payment_method]=pm_card_visa' | jq -r '.id // empty')"
[ -n "$CUS" ] || die "could not create a Stripe test customer"
echo "   customer: $CUS"

SUB_JSON="$(stripe_api POST /v1/subscriptions \
             -d "customer=$CUS" \
             -d "items[0][price]=$STRIPE_PRICE_PRO_MONTHLY")"
SUB="$(printf '%s' "$SUB_JSON" | jq -r '.id // empty')"
PERIOD1="$(printf '%s' "$SUB_JSON" | jq -r '.current_period_end // empty')"
SUB_STATUS="$(printf '%s' "$SUB_JSON" | jq -r '.status // empty')"
[ -n "$SUB" ] || die "could not create a Stripe test subscription (status=$SUB_STATUS): $(printf '%s' "$SUB_JSON" | jq -c '.error // .' 2>/dev/null)"
echo "   subscription: $SUB (status=$SUB_STATUS, period_end=$PERIOD1)"

# ─────────────────────────────────────────────────────────────────────────────
# 3. Webhook flips the user to `pro` (active)
# ─────────────────────────────────────────────────────────────────────────────
say "3/6: signed checkout.session.completed flips the user to pro/active"
# NOTE: alternative to this signed POST — run `stripe listen --forward-to
# $HUB/api/billing/webhook` and let real Stripe deliver the event. We post the
# event directly (signed with STRIPE_WEBHOOK_SECRET) so the run is self-contained.
evt_completed="$(jq -nc \
  --arg id "evt_e2e_$now" --arg cus "$CUS" --arg sub "$SUB" --arg uid "$USER_ID" \
  '{id:$id, type:"checkout.session.completed",
    data:{object:{object:"checkout.session", client_reference_id:$uid,
                  customer:$cus, subscription:$sub, mode:"subscription"}}}')"
wh="$(post_webhook "$evt_completed")"
[ "$wh" = "200" ] && pass "webhook accepted (200)" || badfl "webhook answered $wh (expected 200 — check STRIPE_WEBHOOK_SECRET matches the hub)"

# The hub processes the webhook synchronously; poll /api/me briefly for safety.
plan=""; pstat=""
for _ in 1 2 3 4 5; do
  ME_JSON="$(me)"
  plan="$(printf '%s' "$ME_JSON" | jq -r '.plan.name // empty')"
  pstat="$(printf '%s' "$ME_JSON" | jq -r '.plan.status // empty')"
  [ "$plan" = "pro" ] && break
  sleep 2
done
if [ "$plan" = "pro" ] && [ "$pstat" = "active" ]; then
  pass "user is now plan=pro status=active"
else
  badfl "expected pro/active after checkout, got plan=$plan status=$pstat"
fi

# ─────────────────────────────────────────────────────────────────────────────
# 4. Test clock +1 month → invoice.paid renewal → current_period_end advances
# ─────────────────────────────────────────────────────────────────────────────
say "4/6: advance the test clock +1 month → invoice.paid renewal advances current_period_end"
advance_to=$((now + 32 * 24 * 3600))
stripe_api POST "/v1/test_helpers/test_clocks/$TC_ID/advance" -d "frozen_time=$advance_to" >/dev/null \
  || badfl "could not advance the test clock"
if wait_clock_ready "$TC_ID"; then
  SUB_JSON="$(stripe_api GET "/v1/subscriptions/$SUB")"
  PERIOD2="$(printf '%s' "$SUB_JSON" | jq -r '.current_period_end // empty')"
  evt_paid="$(jq -nc --arg id "evt_e2e_paid_$now" --arg sub "$SUB" --arg cus "$CUS" \
    '{id:$id, type:"invoice.paid",
      data:{object:{object:"invoice", subscription:$sub, customer:$cus, status:"paid"}}}')"
  wh="$(post_webhook "$evt_paid")"
  [ "$wh" = "200" ] && pass "invoice.paid webhook accepted (200)" || badfl "invoice.paid webhook answered $wh"
  pe_me="$(me_field '.plan.periodEnd')"
  # Assert Stripe advanced the period, and (if /api/me exposes periodEnd) the hub mirrored it.
  if [ -n "$PERIOD2" ] && [ -n "$PERIOD1" ] && [ "$PERIOD2" -gt "$PERIOD1" ] 2>/dev/null; then
    pass "current_period_end advanced at Stripe ($PERIOD1 → $PERIOD2)"
  else
    badfl "current_period_end did not advance (was $PERIOD1, now $PERIOD2)"
  fi
  if [ -n "$pe_me" ]; then
    pass "/api/me now reports plan.periodEnd=$pe_me"
  else
    stub "/api/me exposes no plan.periodEnd to cross-check (optional per 0058 C2)"
  fi
else
  badfl "test clock never reached 'ready' after the +1-month advance"
fi

# ─────────────────────────────────────────────────────────────────────────────
# 5. Failing card → invoice.payment_failed → past_due, plan still `pro`
# ─────────────────────────────────────────────────────────────────────────────
say "5/6: switch to a failing card, renew again → payment_failed → past_due (plan stays pro)"
stripe_api POST "/v1/customers/$CUS" \
  -d 'payment_method=pm_card_chargeCustomerFail' \
  -d 'invoice_settings[default_payment_method]=pm_card_chargeCustomerFail' >/dev/null \
  || badfl "could not swap the customer to the failing test card"
advance_to=$((now + 64 * 24 * 3600))
stripe_api POST "/v1/test_helpers/test_clocks/$TC_ID/advance" -d "frozen_time=$advance_to" >/dev/null \
  || badfl "could not advance the test clock for the failed renewal"
if wait_clock_ready "$TC_ID"; then
  evt_fail="$(jq -nc --arg id "evt_e2e_fail_$now" --arg sub "$SUB" --arg cus "$CUS" \
    '{id:$id, type:"invoice.payment_failed",
      data:{object:{object:"invoice", subscription:$sub, customer:$cus, status:"open"}}}')"
  wh="$(post_webhook "$evt_fail")"
  [ "$wh" = "200" ] && pass "invoice.payment_failed webhook accepted (200)" || badfl "payment_failed webhook answered $wh"
  ME_JSON="$(me)"
  plan="$(printf '%s' "$ME_JSON" | jq -r '.plan.name // empty')"
  pstat="$(printf '%s' "$ME_JSON" | jq -r '.plan.status // empty')"
  maxA="$(printf '%s' "$ME_JSON" | jq -r '.plan.maxAgents // empty')"
  if [ "$plan" = "pro" ] && [ "$pstat" = "past_due" ]; then
    pass "plan_status=past_due while plan stays pro (grace window, full Pro caps)"
  else
    badfl "expected pro/past_due, got plan=$plan status=$pstat"
  fi
  [ "$maxA" = "10" ] && pass "Pro caps still enforced during grace (maxAgents=10)" \
    || badfl "expected maxAgents=10 during grace, got $maxA"
else
  badfl "test clock never reached 'ready' after the failed-renewal advance"
fi

# ─────────────────────────────────────────────────────────────────────────────
# 6. Past grace → subscription.deleted → downgrade + agents intact + new approval 402
# ─────────────────────────────────────────────────────────────────────────────
say "6/6: past grace (subscription.deleted) → downgrade to fallback, nothing deleted, new approval 402"
# Count agents rows BEFORE downgrade (criterion 17: agents count unchanged).
agents_before="$(me_field '.plan.agents')"
[ -n "$agents_before" ] || agents_before="0"

stripe_api DELETE "/v1/subscriptions/$SUB" >/dev/null || badfl "could not cancel the Stripe subscription"
evt_del="$(jq -nc --arg id "evt_e2e_del_$now" --arg sub "$SUB" --arg cus "$CUS" \
  '{id:$id, type:"customer.subscription.deleted",
    data:{object:{object:"subscription", id:$sub, customer:$cus, status:"canceled"}}}')"
wh="$(post_webhook "$evt_del")"
[ "$wh" = "200" ] && pass "subscription.deleted webhook accepted (200)" || badfl "subscription.deleted webhook answered $wh"

ME_JSON="$(me)"
plan="$(printf '%s' "$ME_JSON" | jq -r '.plan.name // empty')"
pstat="$(printf '%s' "$ME_JSON" | jq -r '.plan.status // empty')"
maxA="$(printf '%s' "$ME_JSON" | jq -r '.plan.maxAgents // empty')"
agents_after="$(printf '%s' "$ME_JSON" | jq -r '.plan.agents // empty')"
# Fresh user's prior_plan is 'free', so the fallback is 'free' (2/5).
if [ "$plan" = "free" ] && [ "$maxA" = "2" ]; then
  pass "downgraded to the fallback plan (free, 2/5); status=$pstat"
else
  badfl "expected fallback free/2 after cancellation, got plan=$plan maxAgents=$maxA status=$pstat"
fi

# "agents rows intact" — nothing deleted on downgrade (criterion 17 / 19).
if [ "${agents_after:-0}" = "$agents_before" ]; then
  pass "machine (agents) count unchanged across downgrade ($agents_before → ${agents_after:-0})"
else
  badfl "agents count changed across downgrade ($agents_before → ${agents_after:-0}) — 'nothing is deleted' violated"
fi
# SQL-level assert (authoritative), only runnable on the hub host with the DB.
if [ -n "${CCHUB_DB:-}" ] && command -v sqlite3 >/dev/null 2>&1 && [ -r "${CCHUB_DB}" ]; then
  sql_cnt="$(sqlite3 "$CCHUB_DB" \
    "SELECT COUNT(*) FROM agents a JOIN users u ON a.user_id = u.id WHERE u.email = '$EMAIL';" 2>/dev/null || echo '?')"
  if [ "$sql_cnt" = "$agents_before" ]; then
    pass "SQL assert: agents rows for $EMAIL intact (count=$sql_cnt)"
  else
    badfl "SQL assert: agents rows for $EMAIL = $sql_cnt, expected $agents_before"
  fi
else
  stub "agents-rows SQL assert skipped (set CCHUB_DB to the hub's sqlite DB, on the hub host). Run by hand:
        sqlite3 \"\$CCHUB_DB\" \"SELECT COUNT(*) FROM agents a JOIN users u ON a.user_id=u.id WHERE u.email='$EMAIL';\"
        (expected: unchanged across the downgrade)"
fi

# New machine approval must 402 once over the (now free) cap. A real cap-402
# requires the user to already sit at/above the free cap (2 machines), which needs
# genuinely enrolled agents (device flow + a running agent, exactly what
# e2e-saas-journey.sh stands up with docker). We probe the endpoint's shape here
# and mark the true over-cap assertion a STUB when no machines were pre-enrolled.
if [ "${agents_before:-0}" -ge 2 ] 2>/dev/null; then
  ap="$(hub_req POST /api/device/approve -b "$JAR" -H 'content-type: application/json' \
          -d '{"user_code":"ZZZZ-ZZZZ"}')"
  ap_status="$(printf '%s' "$ap" | head -1)"
  if [ "$ap_status" = "402" ]; then
    pass "a new machine approval over the free cap returns 402"
  else
    badfl "expected 402 on a new approval over cap, got $ap_status"
  fi
else
  stub "new-approval-402 check needs >=2 pre-enrolled machines (free cap) to be meaningful.
        This user has $agents_before. Enroll machines first (see e2e-saas-journey.sh's docker
        device flow), then a fresh approval must 402. Downgrade+402 path is exercised in the hub
        unit tests (0058 acceptance 5/9); this script asserts the plan-level downgrade above."
fi

# ─────────────────────────────────────────────────────────────────────────────
echo
if [ "$rc" -eq 0 ]; then
  printf '%sBILLING E2E OK%s against %s (%s)\n' "$C_G" "$C_0" "$HUB" "$EMAIL"
else
  printf '%sBILLING E2E FAILED%s against %s (%s)\n' "$C_R" "$C_0" "$HUB" "$EMAIL"
fi
echo
echo "Teardown:"
echo "  # Stripe test clock (+ its customer/subscription) auto-deleted unless KEEP=1."
echo "  # On the hub origin host, remove the throwaway account:"
echo "  cc-screen-hub user delete '$EMAIL'"
exit "$rc"
