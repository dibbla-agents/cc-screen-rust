#!/usr/bin/env bash
# release.sh — cut a cc-screen-rust release.
#
# The dist workflow builds all cross-platform artifacts on a version tag, but the
# dibbla-agents org enforces a read-only Actions GITHUB_TOKEN, so the workflow's
# own "Create GitHub Release" step 403s. This script tags, waits for the CI build,
# then publishes the release from the built artifacts using your (user) gh creds.
# If the org setting is ever fixed and CI publishes the release itself, this
# detects that and just no-ops.
#
#   ./release.sh             version from Cargo.toml -> tag vX.Y.Z, then publish
#   ./release.sh v0.2.0      explicit tag
#   ./release.sh --force     delete + recreate the release if it already exists
#   ./release.sh --dry-run   do everything except create/delete the release
set -euo pipefail
cd "$(dirname "$0")"

REPO="dibbla-agents/cc-screen-rust"
FORCE=0; DRYRUN=0; TAG=""
for a in "$@"; do
  case "$a" in
    --force)   FORCE=1 ;;
    --dry-run) DRYRUN=1 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    v[0-9]*)   TAG="$a" ;;
    *) echo "unknown arg: $a (try --help)" >&2; exit 1 ;;
  esac
done

command -v gh >/dev/null 2>&1 || { echo "gh CLI is required" >&2; exit 1; }
gh auth status >/dev/null 2>&1 || { echo "gh is not authenticated (run: gh auth login)" >&2; exit 1; }

# ── 1. resolve the tag ──────────────────────────────────────────────────────
if [ -z "$TAG" ]; then
  ver="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
  [ -n "$ver" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
  TAG="v$ver"
fi
echo "→ release tag: $TAG"

# ── 2. create + push the tag (skipped on --dry-run) ─────────────────────────
if ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  if [ "$DRYRUN" = 1 ]; then
    echo "  tag $TAG does not exist — dry-run needs an existing tag/CI run; aborting." >&2
    exit 1
  fi
  echo "  creating annotated tag $TAG"
  git tag -a "$TAG" -m "$TAG"
fi
if [ "$DRYRUN" != 1 ]; then
  git push origin "$TAG" 2>&1 | sed 's/^/  /' || true
fi
COMMIT="$(git rev-list -n1 "$TAG")"
echo "  tagged commit: $COMMIT"

# ── 3. find the Release workflow run for this commit (wait for it to appear) ─
echo "→ locating the Release workflow run for $COMMIT …"
RID=""
for _ in $(seq 1 30); do
  RID="$(gh run list -R "$REPO" --workflow=release.yml --event push --limit 30 \
        --json databaseId,headSha \
        --jq "map(select(.headSha==\"$COMMIT\")) | .[0].databaseId" 2>/dev/null || true)"
  [ -n "$RID" ] && [ "$RID" != "null" ] && break
  RID=""; sleep 5
done
[ -n "$RID" ] || { echo "no release.yml run found for $COMMIT (did the tag push trigger CI?)" >&2; exit 1; }
echo "  run id: $RID"

# ── 4. wait for the build to finish (host job will fail on the org token —
#       expected; the build artifacts upload before that step) ───────────────
echo "→ waiting for the build (the 'host' job failing is expected) …"
gh run watch "$RID" -R "$REPO" || true

# ── 5. if CI already published the release (org setting fixed), we're done ───
existing="$(gh release view "$TAG" -R "$REPO" --json assets --jq '.assets|length' 2>/dev/null || echo 0)"
if [ "${existing:-0}" -gt 0 ] && [ "$FORCE" != 1 ] && [ "$DRYRUN" != 1 ]; then
  echo "→ release $TAG already exists with $existing assets — nothing to do."
  echo "  (CI may have published it, or it was published earlier. Use --force to rebuild it.)"
  exit 0
fi

# ── 6. confirm the build jobs succeeded, then download the artifacts ────────
if gh run view "$RID" -R "$REPO" --json jobs \
     --jq '[.jobs[]|select(.name|startswith("build-"))|.conclusion]|any(.=="failure")' \
     2>/dev/null | grep -q true; then
  echo "a build job failed — aborting. Inspect: gh run view $RID -R $REPO" >&2
  exit 1
fi

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
echo "→ downloading build artifacts …"
gh run download "$RID" -R "$REPO" -D "$TMP" >/dev/null

# ── 7. assemble the release file set (mirrors what dist's host job uploads) ──
mkdir "$TMP/rel"
cp "$TMP"/artifacts-build-local-*/*.tar.xz        "$TMP/rel/" 2>/dev/null || true
cp "$TMP"/artifacts-build-local-*/*.tar.xz.sha256 "$TMP/rel/" 2>/dev/null || true
cp "$TMP"/artifacts-build-global/*-installer.sh \
   "$TMP"/artifacts-build-global/sha256.sum \
   "$TMP"/artifacts-build-global/source.tar.gz \
   "$TMP"/artifacts-build-global/source.tar.gz.sha256 "$TMP/rel/" 2>/dev/null || true
cp "$TMP"/artifacts-dist-manifest/dist-manifest.json "$TMP/rel/" 2>/dev/null || true

count="$(ls -1 "$TMP/rel" | wc -l | tr -d ' ')"
tarballs="$(ls -1 "$TMP"/rel/*.tar.xz 2>/dev/null | grep -vc source || true)"
echo "  staged $count files ($tarballs platform tarballs)"
[ "${tarballs:-0}" -ge 8 ] || { echo "expected >=8 platform tarballs, found ${tarballs:-0} — aborting." >&2; exit 1; }

# ── 8. release notes: dist's generated body if present, else the tag ────────
title="$TAG"; notes="$TMP/notes.md"; : > "$notes"
if [ -f "$TMP/rel/dist-manifest.json" ] && command -v python3 >/dev/null 2>&1; then
  title="$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(m.get("announcement_title") or sys.argv[2])' "$TMP/rel/dist-manifest.json" "$TAG" 2>/dev/null || echo "$TAG")"
  python3 -c 'import json,sys; open(sys.argv[2],"w").write(json.load(open(sys.argv[1])).get("announcement_github_body") or "")' "$TMP/rel/dist-manifest.json" "$notes" 2>/dev/null || true
fi
[ -s "$notes" ] || printf '%s\n' "$TAG" > "$notes"

# ── 9. publish ──────────────────────────────────────────────────────────────
if [ "$DRYRUN" = 1 ]; then
  echo "→ [dry-run] would publish $TAG (title: \"$title\") with these assets:"
  ls -1 "$TMP/rel" | sed 's/^/    /'
  exit 0
fi
if [ "$FORCE" = 1 ] && [ "${existing:-0}" -gt 0 ]; then
  echo "→ --force: deleting existing release $TAG (tag kept) …"
  gh release delete "$TAG" -R "$REPO" --yes
fi
echo "→ publishing release $TAG …"
gh release create "$TAG" -R "$REPO" \
  --title "$title" --notes-file "$notes" --verify-tag --latest \
  "$TMP"/rel/*
echo "✓ published: https://github.com/$REPO/releases/tag/$TAG"

# ── 10. verify the anonymous install URLs (retry past edge propagation) ─────
echo "→ verifying anonymous install URLs …"
for f in cc-screen-tui-installer.sh cc-screen-rust-installer.sh; do
  url="https://github.com/$REPO/releases/latest/download/$f"
  code=000
  for _ in $(seq 1 12); do
    code="$(curl --proto '=https' --tlsv1.2 -sIL -o /dev/null -w '%{http_code}' "$url" || echo 000)"
    [ "$code" = "200" ] && break
    sleep 10
  done
  echo "  $f -> HTTP $code"
done
echo "Done."
