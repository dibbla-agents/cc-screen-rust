#!/usr/bin/env bash
# bump.sh X.Y.Z — lockstep version bump across the workspace crates.
#
# cc-screen-rust ships the server, the `ccs` TUI, and the protocol crate as ONE
# version (cargo-dist releases them in lockstep off a vX.Y.Z tag). This edits the
# three [package] version lines, refreshes Cargo.lock, and stages them. It does
# NOT commit, tag, or push — review, then:
#     git commit -m "Release X.Y.Z" && git push
#     ./release.sh && site/release-host.sh
set -euo pipefail
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

NEW="${1:-}"
case "$NEW" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "usage: ./bump.sh X.Y.Z   (e.g. ./bump.sh 0.2.3)" >&2; exit 1 ;;
esac

FILES="Cargo.toml crates/tui/Cargo.toml crates/protocol/Cargo.toml"
OLD="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
[ -n "$OLD" ] || { echo "could not read current version from Cargo.toml" >&2; exit 1; }
if [ "$OLD" = "$NEW" ]; then echo "already at $NEW" >&2; exit 1; fi
echo "→ bump $OLD → $NEW (lockstep)"

# Only the [package] version is at column 0 (dependency versions live inside
# `{ … }`), so a start-of-line anchor is safe and unambiguous.
OLD_ESC="$(printf '%s' "$OLD" | sed 's/[.[\*^$/]/\\&/g')"
for f in $FILES; do
  sed -i "s|^version = \"$OLD_ESC\"|version = \"$NEW\"|" "$f"
  grep -qx "version = \"$NEW\"" "$f" || { echo "failed to bump $f (expected old version $OLD)" >&2; exit 1; }
done

echo "→ refreshing Cargo.lock"
cargo check --workspace >/dev/null

git add $FILES Cargo.lock
echo "✓ bumped to $NEW and staged. Next:"
echo "    git commit -m \"Release $NEW\" && git push"
echo "    ./release.sh && site/release-host.sh"
