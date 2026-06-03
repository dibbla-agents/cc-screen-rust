#!/usr/bin/env bash
# Host a cc-screen release (installers + binaries) on the Dibbla docs site, so
#
#   curl --proto '=https' --tlsv1.2 -LsSf https://<site>/dl/install-ccs.sh | sh
#
# installs off our own domain instead of GitHub Releases. We don't rebuild
# anything: the Release CI (.github/workflows/release.yml) already cross-builds
# all four targets: this pulls those artifacts, rewrites the cargo-dist installers
# to download from the Dibbla origin, stages everything under site/dl/, and
# redeploys the site (the Dockerfile COPYs dl/ → /public/dl).
#
#   ./release-host.sh            # the latest git tag
#   ./release-host.sh v0.2.2     # a specific tag
#
# Needs: gh (authed), dibbla (authed), and a *successful* Release run for the tag.
set -euo pipefail
cd "$(dirname "$0")" # -> site/

SITE_URL="https://cc-screen-b4687da9.dibbla.app"
REPO="dibbla-agents/cc-screen-rust"
GH_BASE="https://github.com/$REPO/releases/download" # what cargo-dist bakes in

command -v gh >/dev/null || { echo "gh not found (needed to fetch CI artifacts)" >&2; exit 1; }

VERSION="${1:-$(git -C .. describe --tags --abbrev=0)}"
echo "→ hosting $VERSION on $SITE_URL/dl"

# The most recent *successful* Release run for this tag (the build jobs succeed
# even if the GitHub-Release publish step doesn't).
RUN_ID="$(gh run list --repo "$REPO" --workflow Release --branch "$VERSION" \
  --status success --json databaseId --jq '.[0].databaseId' 2>/dev/null || true)"
[ -n "$RUN_ID" ] || { echo "no successful Release run found for $VERSION" >&2; exit 1; }
echo "  CI run $RUN_ID"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
gh run download "$RUN_ID" --repo "$REPO" -D "$TMP"

# Stage only this release (bounds the site image size); keep the dir + .gitkeep.
find dl -mindepth 1 -maxdepth 1 ! -name .gitkeep -exec rm -rf {} +
DEST="dl/$VERSION"
mkdir -p "$DEST"
find "$TMP" -name '*.tar.xz' -exec cp {} "$DEST/" \;
find "$TMP" -name '*.tar.xz.sha256' -exec cp {} "$DEST/" \;
echo "  staged $(find "$DEST" -name '*.tar.xz' | wc -l | tr -d ' ') tarballs in $DEST"

# Rewrite the cargo-dist installers: GitHub download URL -> Dibbla origin. The
# embedded checksums and OS/arch/musl detection are untouched, so they keep
# verifying — they just fetch the tarballs from us.
sed "s#$GH_BASE#$SITE_URL/dl#g" "$TMP/artifacts-build-global/cc-screen-tui-installer.sh"  > dl/install-ccs.sh
sed "s#$GH_BASE#$SITE_URL/dl#g" "$TMP/artifacts-build-global/cc-screen-rust-installer.sh" > dl/install-cc-screen.sh
chmod +x dl/install-ccs.sh dl/install-cc-screen.sh
printf '%s\n' "$VERSION" > dl/version

# Sanity: the rewritten installers must point at us, not GitHub.
if grep -q "github.com/$REPO/releases/download" dl/install-ccs.sh dl/install-cc-screen.sh; then
  echo "ERROR: a github download URL survived the rewrite" >&2; exit 1
fi
echo "  installers rewritten → $SITE_URL/dl"

# Deploy (builds docs -> public; the Dockerfile layers dl/ -> /public/dl).
echo "→ deploying the site…"
./deploy.sh "host release $VERSION"

cat <<EOF

✓ hosted $VERSION. Install one-liners:
    ccs    : curl --proto '=https' --tlsv1.2 -LsSf $SITE_URL/dl/install-ccs.sh | sh
    server : curl --proto '=https' --tlsv1.2 -LsSf $SITE_URL/dl/install-cc-screen.sh | sh
EOF
