#!/usr/bin/env bash
# Build the frontend, then the Rust binary that embeds it.
#   ./build.sh build   frontend -> frontend/dist -> cargo build --release
#   ./build.sh fe       just the frontend
#   ./build.sh be       just the backend (assumes frontend/dist exists)
#   ./build.sh run      build + run (foreground)
set -euo pipefail
cd "$(dirname "$0")"

# Make cargo available even from a minimal shell.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

fe() {
  echo "── frontend ──"
  ( cd frontend && { [ -d node_modules ] || npm ci; } && npm run build )
}
be() {
  echo "── backend ──"
  cargo build --release
}

case "${1:-build}" in
  fe) fe ;;
  be) be ;;
  build) fe; be; echo "built ./target/release/cc-screen-rust" ;;
  run) fe; be; ./target/release/cc-screen-rust ;;
  *) echo "usage: $0 {build|fe|be|run}" >&2; exit 2 ;;
esac
