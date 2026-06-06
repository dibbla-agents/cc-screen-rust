#!/usr/bin/env bash
# cc-screen-rust clipboard shim (proposal 0007).
#
# Installed as xclip / wl-paste / pbpaste in ~/.local/bin, which the agent puts
# first on every session's PATH (see src/config.rs build_env_path). Claude Code
# shells out to these tools to read a clipboard image when you press Ctrl-V; this
# shim answers IMAGE queries from, in priority order (first hit wins):
#
#   1. THIS Rust agent's session  — $CCWEB_CLIP_URL, scoped by $CCWEB_SESSION
#   2. the legacy Go cc-screen-web — ~/.config/cc-screen/web.env  (CCWEB_ADDR)
#   3. the macOS clip-server       — http://127.0.0.1:9999 (SSH RemoteForward)
#
# so a phone-pasted screenshot lands whichever server staged it, and a Mac
# clipboard image still pastes — none of the previously-working sources regress.
#
# Anything that is NOT an image query (text copy/paste, -selection, -o/-i, a
# text `wl-paste --list-types`) is delegated to the REAL tool: the next match on
# PATH after this shim. We resolve it at runtime with `type -aP` (every PATH
# match, in order) minus this shim itself — no install-time state, so it keeps
# working if the real tool moves.
#
# This file is the single source of truth: it is embedded into the agent binary
# (include_str! in src/service.rs) and written out, byte-for-byte, as all three
# tool names by `cc-screen-rust install` / `cc-screen-rust install-shim`. The
# invoked name ($0's basename) selects the dispatch below.
set -u

self="$(basename -- "$0")"

# ── the real tool: the next PATH match that isn't this shim ───────────────────
real_tool() {
  local me cand rcand
  me="$(readlink -f -- "$0" 2>/dev/null || printf '%s' "$0")"
  while IFS= read -r cand; do
    [ -n "$cand" ] || continue
    rcand="$(readlink -f -- "$cand" 2>/dev/null || printf '%s' "$cand")"
    [ "$rcand" = "$me" ] && continue   # skip ourselves
    printf '%s\n' "$cand"
    return 0
  done < <(type -aP -- "$self" 2>/dev/null)
  return 1
}

# Hand off to the real tool (text paste, copy, etc.); if none exists, do nothing
# — same as the absence of a clipboard, never an error.
defer() {
  local real
  real="$(real_tool)" || exit 0
  exec "$real" "$@"
}

# ── image sources, probed in priority order ───────────────────────────────────
GO_WEB=""
go_cfg="${XDG_CONFIG_HOME:-$HOME/.config}/cc-screen/web.env"
[ -f "$go_cfg" ] && GO_WEB="$(sed -n 's/^CCWEB_ADDR=//p' "$go_cfg" | head -1)"
MAC_PORT=9999

# True if the URL's /targets probe reports an image is available.
has_image() { curl -fsS --max-time 1 "$1" 2>/dev/null | grep -q image; }

rust_q() { [ -n "${CCWEB_SESSION:-}" ] && printf '?session=%s' "$CCWEB_SESSION"; }

# Echo the image-fetch URL of the first source that has a staged image, or
# return 1 if none does (→ caller defers to the real local clipboard).
image_url() {
  if [ -n "${CCWEB_CLIP_URL:-}" ] \
     && has_image "${CCWEB_CLIP_URL}/api/clip/targets$(rust_q)"; then
    printf '%s/api/clip/image.png%s\n' "$CCWEB_CLIP_URL" "$(rust_q)"
    return 0
  fi
  if [ -n "$GO_WEB" ] && has_image "http://$GO_WEB/api/clip/targets"; then
    printf 'http://%s/api/clip/image.png\n' "$GO_WEB"
    return 0
  fi
  if has_image "http://127.0.0.1:$MAC_PORT/targets"; then
    printf 'http://127.0.0.1:%s/image/png\n' "$MAC_PORT"
    return 0
  fi
  return 1
}

# Answer a "what targets are available?" probe.
emit_targets() {
  if image_url >/dev/null; then printf 'image/png\n'; else defer "$@"; fi
}

# Answer an "give me the image bytes" probe.
emit_image() {
  local url
  if url="$(image_url)"; then curl -fsS --max-time 5 "$url" 2>/dev/null; else defer "$@"; fi
}

# ── dispatch by the name we were invoked as ───────────────────────────────────
case "$self" in
  xclip)
    # xclip names its selection target with `-t <target>`; TARGETS lists types.
    target=""; seen_t=0
    for arg in "$@"; do
      if [ "$seen_t" = 1 ]; then target="$arg"; seen_t=0; continue; fi
      [ "$arg" = "-t" ] && seen_t=1
    done
    case "$target" in
      TARGETS)  emit_targets "$@" ;;
      image/*)  emit_image "$@" ;;
      *)        defer "$@" ;;
    esac
    ;;
  wl-paste)
    # `wl-paste -l|--list-types` lists types; `-t image/...` requests the image.
    case " $* " in
      *" -l "*|*" --list-types "*) emit_targets "$@" ;;
      *image/*)                    emit_image "$@" ;;
      *)                           defer "$@" ;;
    esac
    ;;
  pbpaste)
    # pbpaste has no target flags: serve a staged image if one exists this
    # session, otherwise hand off to the real pbpaste for ordinary text.
    if image_url >/dev/null; then emit_image "$@"; else defer "$@"; fi
    ;;
  *)
    defer "$@" ;;
esac
