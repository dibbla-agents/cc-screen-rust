#!/usr/bin/env bash
# Deploy the cc-screen docs site to Dibbla as the `cc-screen` app.
#
# docs/ is the single source of truth (also served by GitHub Pages); this syncs
# it into ./public, then deploys this tiny static server. First run creates the
# app; later runs do a zero-downtime rolling update.
#
#   ./deploy.sh ["commit message"]
set -euo pipefail
cd "$(dirname "$0")"

# 1. sync the canonical docs into the deploy unit
rm -rf public && mkdir public
cp -R ../docs/. public/

# 2. first deploy creates the app; subsequent ones update in place
msg="${1:-deploy: cc-screen docs site}"
if dibbla apps list 2>/dev/null | grep -qE '^cc-screen[[:space:]]'; then
  dibbla deploy . --alias cc-screen --port 8080 --update -m "$msg"
else
  dibbla deploy . --alias cc-screen --port 8080 -m "$msg"
fi
