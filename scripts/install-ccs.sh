#!/bin/sh
# ccs installer, served by the hub at GET /ccs.sh (proposal 0060 D3).
#
#   curl -fsSL <hub>/ccs.sh | sh
#
# Wraps the generic cargo-dist installer for the `ccs` terminal client and ends
# by PRINTING the sign-in command (never running it — this pipe has no TTY).
# __CCSCREEN_HUB_URL__ / __CCSCREEN_INSTALLER_URL__ are substituted per request
# from the hub's own origin, so the printed next step already points home.
set -eu

HUB_URL="__CCSCREEN_HUB_URL__"
INSTALLER_URL="__CCSCREEN_INSTALLER_URL__"

echo "-> installing ccs (the cc-screen terminal client)"
curl --proto '=https' --tlsv1.2 -LsSf "$INSTALLER_URL" | sh

echo ""
echo "ccs installed. Now sign this terminal in:"
echo ""
echo "  ccs activate --server $HUB_URL"
echo ""
echo "(prints a one-time code; approve it from any logged-in browser — your phone works)"
