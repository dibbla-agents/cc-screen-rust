#!/usr/bin/env bash
# e2e-link-share — read-only link grants against a LIVE multi-tenant hub
# (proposal 0083 Part D).
#
# `crates/hub/tests/link_shares.rs` proves the same properties hermetically in
# CI, against a fake agent. This script proves them on the *deployed* thing —
# a real account, a real machine, a real file on a real disk, through whatever
# reverse proxy and CDN sit in front of the hub. That last part is the reason it
# exists: `no-store`, `nosniff` and the absence of an `ETag` are only true if
# nothing in the path adds or strips them.
#
# Usage:
#   HUB=https://app.ccscreen.dev EMAIL=you@example.com PASSWORD=… \
#   MACHINE=studio FILE=/home/you/cc-share/e2e-link.md \
#     scripts/e2e-link-share.sh
#
# The account must be able to create shares (Pro or a team seat) and must own
# MACHINE, which must be ONLINE — a link cannot be minted against a machine
# that can't canonicalize the path. FILE must exist and be text; the script
# never writes to your disk.
#
# Every grant it mints, it revokes. A run that dies mid-way leaves at most two
# link grants in ~/shared — revoke them from the dashboard.
set -eu

HUB="${HUB:-https://app.ccscreen.dev}"
: "${EMAIL:?set EMAIL}"
: "${PASSWORD:?set PASSWORD}"
: "${MACHINE:?set MACHINE (the machine label that owns FILE)}"
: "${FILE:?set FILE (an absolute path to a text file on MACHINE)}"

JAR="$(mktemp)"
BODY="$(mktemp)"
HDRS="$(mktemp)"
trap 'rm -f "$JAR" "$BODY" "$HDRS"' EXIT

say()  { printf '\n== %s\n' "$*"; }
ok()   { printf '   ok  %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# "<status>" to stdout, body in $BODY, headers in $HDRS.
req() {
  method="$1"; path="$2"; shift 2
  curl -sS -o "$BODY" -D "$HDRS" -w '%{http_code}' -X "$method" "$HUB$path" "$@" || true
}
# A header value, lowercased name, or "" when absent.
hdr() { tr -d '\r' < "$HDRS" | awk -v n="$(printf '%s' "$1" | tr 'A-Z' 'a-z')" \
        'BEGIN{IGNORECASE=1} tolower($1) == n":" { $1=""; sub(/^ /,""); print }'; }
# The full response, normalised for comparison: status line + the headers we
# contract on + body. Anything that legitimately varies (Date) is dropped.
snapshot() {
  printf 'status=%s\n' "$1"
  for h in content-type x-content-type-options cache-control referrer-policy x-robots-tag etag last-modified; do
    printf '%s=%s\n' "$h" "$(hdr "$h")"
  done
  printf 'body=%s\n' "$(cat "$BODY")"
}

say "sign in as $EMAIL"
st="$(req POST /api/login "-H" "content-type: application/json" \
        -d "{\"password\":\"$PASSWORD\",\"email\":\"$EMAIL\"}" -c "$JAR")"
[ "$st" = "200" ] || fail "login: $st $(cat "$BODY")"
ok "logged in"

say "mint a read-only link for $FILE on $MACHINE"
st="$(req POST /api/shares -H 'content-type: application/json' -b "$JAR" \
        -d "{\"kind\":\"link\",\"machine\":\"$MACHINE\",\"path\":\"$FILE\"}")"
[ "$st" = "200" ] || fail "mint: $st $(cat "$BODY")"
URL="$(sed -n 's/.*"invite_url":"\([^"]*\)".*/\1/p' "$BODY")"
ID="$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' "$BODY")"
TOKEN="${URL##*/}"
[ -n "$TOKEN" ] && [ -n "$ID" ] || fail "mint returned no url/id: $(cat "$BODY")"
ok "minted $URL"

say "the token is nowhere in the outbox (hashed at rest)"
st="$(req GET /api/shares/outbox -b "$JAR")"
[ "$st" = "200" ] || fail "outbox: $st"
grep -q "$TOKEN" "$BODY" && fail "the outbox leaked the token"
ok "outbox lists the grant, not the token"

say "an anonymous read serves the file under the full response contract"
st="$(req GET "/api/link/$TOKEN/content")"
[ "$st" = "200" ] || fail "anonymous read: $st $(cat "$BODY")"
[ -s "$BODY" ] || fail "empty body"
[ "$(hdr content-type)" = "text/plain; charset=utf-8" ] || fail "content-type: $(hdr content-type)"
[ "$(hdr x-content-type-options)" = "nosniff" ] || fail "missing nosniff"
[ "$(hdr cache-control)" = "no-store" ] || fail "cache-control: $(hdr cache-control)"
[ "$(hdr referrer-policy)" = "no-referrer" ] || fail "referrer-policy: $(hdr referrer-policy)"
[ -n "$(hdr x-robots-tag)" ] || fail "missing x-robots-tag"
[ -z "$(hdr etag)" ] || fail "an ETag survived the proxy — a 304 would outlive a revoke"
[ -z "$(hdr last-modified)" ] || fail "a Last-Modified survived the proxy"
ok "$(wc -c < "$BODY") bytes, no validators, no cache"

say "no endpoint takes the token for a write or a listing"
for probe in \
  "POST /api/file/write" \
  "GET /api/files?machine=$MACHINE" \
  "GET /api/sessions"
do
  set -- $probe
  st="$(req "$1" "$2" -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' \
          -d "{\"path\":\"$FILE\",\"content\":\"pwned\",\"baseMtime\":0}")"
  [ "$st" = "401" ] || [ "$st" = "403" ] || fail "$probe answered $st with a link token"
done
ok "every write/listing probe refused"

say "revoke, then every refusal is byte-identical"
st="$(req POST "/api/shares/$ID/revoke" -b "$JAR")"
[ "$st" = "200" ] || fail "revoke: $st $(cat "$BODY")"

st="$(req GET "/api/link/$TOKEN/content")"; REVOKED="$(snapshot "$st")"
st="$(req GET "/api/link/$(printf 'A%.0s' $(seq 43))/content")"; UNKNOWN="$(snapshot "$st")"
st="$(req GET "/api/link/nope/content")"; MALFORMED="$(snapshot "$st")"
[ "$REVOKED" = "$UNKNOWN" ] || fail "revoked and unknown differ:
$(diff <(printf '%s' "$REVOKED") <(printf '%s' "$UNKNOWN") || true)"
[ "$REVOKED" = "$MALFORMED" ] || fail "revoked and malformed differ:
$(diff <(printf '%s' "$REVOKED") <(printf '%s' "$MALFORMED") || true)"
printf '%s' "$REVOKED" | grep -q '^status=404$' || fail "the refusal is not a 404: $REVOKED"
ok "revoked / unknown / malformed are one answer"

say "regenerate kills the old URL and keeps the grant"
st="$(req POST /api/shares -H 'content-type: application/json' -b "$JAR" \
        -d "{\"kind\":\"link\",\"machine\":\"$MACHINE\",\"path\":\"$FILE\"}")"
[ "$st" = "200" ] || fail "second mint: $st $(cat "$BODY")"
ID2="$(sed -n 's/.*"id":"\([^"]*\)".*/\1/p' "$BODY")"
OLD="$(sed -n 's/.*"invite_url":"\([^"]*\)".*/\1/p' "$BODY")"; OLD="${OLD##*/}"
st="$(req POST "/api/shares/$ID2/regenerate" -b "$JAR")"
[ "$st" = "200" ] || fail "regenerate: $st $(cat "$BODY")"
NEW="$(sed -n 's/.*"invite_url":"\([^"]*\)".*/\1/p' "$BODY")"; NEW="${NEW##*/}"
[ "$OLD" != "$NEW" ] || fail "regenerate returned the same token"
[ "$(req GET "/api/link/$OLD/content")" = "404" ] || fail "the old URL still serves"
[ "$(req GET "/api/link/$NEW/content")" = "200" ] || fail "the new URL doesn't serve"
ok "old URL dead, new URL live, same grant id"

say "clean up"
[ "$(req POST "/api/shares/$ID2/revoke" -b "$JAR")" = "200" ] || fail "cleanup revoke failed"
ok "revoked $ID2"

printf '\nPASS — link grants behave on %s\n' "$HUB"
