#!/usr/bin/env bash
# Deploy the cc-screen landing site to Dibbla as the `cc-screen` app.
#
# The site is a Vite + React + TypeScript + Tailwind app in ./web. We build it
# (Vite emits into ../docs — the same tree GitHub Pages serves), sync that build
# into ./public, then deploy this tiny static server. First run creates the app;
# later runs do a zero-downtime rolling update.
#
# Since proposal 0054 the canonical host is https://ccscreen.dev (GitHub Pages
# custom domain); the Dibbla alias survives only as a redirector. SITE_CANONICAL
# is exported to the app, and when set the server 301s every request to
# ${SITE_CANONICAL}${path} instead of serving (see src/main.rs). Override with
# SITE_CANONICAL="" to make the alias serve the site again.
#
#   ./deploy.sh ["commit message"]
set -euo pipefail
cd "$(dirname "$0")"

SITE_CANONICAL="${SITE_CANONICAL-https://ccscreen.dev}"
export SITE_CANONICAL

# 1. build the site → ../docs
( cd web && npm install --no-audit --no-fund && npm run build )

# 2. sync the built site into the deploy unit
rm -rf public && mkdir public
cp -R ../docs/. public/

# Cache-busting is automatic: Vite content-hashes every emitted asset
# (index-<hash>.js, index-<hash>.css, <name>-<hash>.png), so a changed file gets
# a new filename → a new CDN cache key → the update ships immediately, while
# unchanged assets keep their long cache. index.html itself is served uncached
# (cf-cache-status: DYNAMIC) and points at the current hashes. No manual ?v= stamp
# needed (this script used to sed one onto styles.css/app.js, which no longer exist).

# 3. first deploy creates the app; subsequent ones update in place
msg="${1:-deploy: cc-screen docs site}"
if dibbla apps list 2>/dev/null | grep -qE '^cc-screen[[:space:]]'; then
  dibbla deploy . --alias cc-screen --port 8080 --update \
    -e "SITE_CANONICAL=$SITE_CANONICAL" -m "$msg"
else
  dibbla deploy . --alias cc-screen --port 8080 \
    -e "SITE_CANONICAL=$SITE_CANONICAL" -m "$msg"
fi
