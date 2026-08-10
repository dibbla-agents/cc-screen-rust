#!/usr/bin/env bash
# cc-screen-rust clipboard shim (proposals 0007 and 0077 D1).
#
# It shims BOTH directions, and they are different jobs:
#
#   IN  (0007) — xclip / wl-paste / pbpaste. Claude Code shells out to these to
#       READ a clipboard image when you press Ctrl-V; we answer with whatever
#       the web UI staged for this session.
#   OUT (0077 D1) — pbcopy / wl-copy. When something inside a session COPIES
#       text, the copy belongs on the clipboard of the machine the USER is
#       sitting at, not on this one. Writing it here is worse than losing it: on
#       a Mac agent the write succeeds, the assistant reports "copied to
#       clipboard", and the text sits on a shared machine's pasteboard the user
#       is not at. So we deliver it in-band as OSC 52 on the session's own
#       terminal, which the agent relays verbatim and the attached client (the
#       web app or `ccs`) turns into a real clipboard write. Guarded on
#       $CCWEB_SESSION: ~/.local/bin is on the machine owner's INTERACTIVE PATH
#       too, and their own `pbcopy` must keep working exactly as before.
#
# Installed in ~/.local/bin, which the agent puts first on every session's PATH
# (see src/config.rs build_env_path). The read side answers IMAGE queries from,
# in priority order (first hit wins):
#
#   1. THIS session's local drop file — $CCWEB_CLIP_FILE (works even when the
#      agent is hub-only and binds no HTTP port; the agent writes it on stage)
#   2. THIS Rust agent over HTTP       — $CCWEB_CLIP_URL, scoped by $CCWEB_SESSION
#   3. the legacy Go cc-screen-web     — ~/.config/cc-screen/web.env (CCWEB_ADDR)
#   4. the macOS clip-server           — http://127.0.0.1:9999 (SSH RemoteForward)
#
# so a phone-pasted screenshot lands whichever server staged it, and a Mac
# clipboard image still pastes — none of the previously-working sources regress.
#
# Anything that is neither an image query nor an in-session text copy (text
# paste, -selection, -o/-i, a text `wl-paste --list-types`, any copy outside a
# cc-screen session) is delegated to the REAL tool: the next match on PATH after
# this shim. We resolve it at runtime with `type -aP` (every PATH match, in
# order) minus this shim itself — no install-time state, so it keeps working if
# the real tool moves.
#
# This file is the single source of truth: it is embedded into the agent binary
# (include_str! in src/service.rs) and written out, byte-for-byte, under every
# name in SHIM_NAMES by `cc-screen-rust install` / `cc-screen-rust install-shim`.
# The invoked name ($0's basename) selects the dispatch below.
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

# Drop-file TTL (seconds) — mirrors the server-side ClipStore TTL so a previous
# paste's file isn't served as the "current" image on a later, non-paste probe.
FILE_TTL=20

# True if $CCWEB_CLIP_FILE exists and was written within FILE_TTL seconds.
file_fresh() {
  [ -n "${CCWEB_CLIP_FILE:-}" ] && [ -f "$CCWEB_CLIP_FILE" ] || return 1
  local now mt age
  now="$(date +%s 2>/dev/null)" || return 1
  mt="$(stat -c %Y "$CCWEB_CLIP_FILE" 2>/dev/null || stat -f %m "$CCWEB_CLIP_FILE" 2>/dev/null)" || return 1
  age=$(( now - mt ))
  [ "$age" -ge 0 ] && [ "$age" -le "$FILE_TTL" ]
}

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
  if file_fresh || image_url >/dev/null; then printf 'image/png\n'; else defer "$@"; fi
}

# Answer an "give me the image bytes" probe.
emit_image() {
  local url
  if file_fresh; then cat "$CCWEB_CLIP_FILE"; return; fi
  if url="$(image_url)"; then curl -fsS --max-time 5 "$url" 2>/dev/null; else defer "$@"; fi
}

# ── copy OUT: deliver a text copy to the user's clipboard (proposal 0077 D1) ──

# Mirrors the clients' cap. Far above any real copy, far below anything that
# would make a terminal emulator unhappy.
COPY_CAP=65536

# Emit one OSC 52 clipboard-store on the session's controlling terminal. That
# terminal is the PTY cc-screen owns, so the sequence rides the normal output
# stream to whichever client is attached — no HTTP, no port, works under
# --hub-only. A client that predates 0077 simply discards it, which is exactly
# what happens today.
emit_copy() {
  local data b64
  data="$1"
  # An empty copy means "clear the clipboard"; we never clear a user's
  # clipboard on remote instruction. Oversize is dropped rather than truncated
  # — a partial copy is worse than none.
  [ -n "$data" ] || exit 0
  [ "${#data}" -le "$COPY_CAP" ] || exit 0
  b64="$(printf '%s' "$data" | base64 | tr -d '\n')" || exit 0
  printf '\033]52;c;%s\007' "$b64" > /dev/tty 2>/dev/null || exit 0
  exit 0
}

# The text a `wl-copy` invocation carries as arguments (it accepts either
# trailing words or stdin). Skips flags and the value of --type/-t.
wl_copy_args() {
  local out="" skip=0 arg
  for arg in "$@"; do
    if [ "$skip" = 1 ]; then skip=0; continue; fi
    case "$arg" in
      -t|--type) skip=1; continue ;;
      -*) continue ;;
    esac
    out="${out:+$out }$arg"
  done
  printf '%s' "$out"
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
    if file_fresh || image_url >/dev/null; then emit_image "$@"; else defer "$@"; fi
    ;;
  pbcopy)
    # Outside a cc-screen session this is the machine owner's own pbcopy —
    # behave exactly like it. Inside one, the copy goes to the user.
    [ -n "${CCWEB_SESSION:-}" ] || defer "$@"
    emit_copy "$(cat)"
    ;;
  wl-copy)
    [ -n "${CCWEB_SESSION:-}" ] || defer "$@"
    # An IMAGE copy is the other direction's business (0007/0066) and not
    # something OSC 52 carries; hand it to the real tool.
    case " $* " in *" image/"*) defer "$@" ;; esac
    # `--clear` empties the clipboard. See emit_copy: we never do that remotely.
    case " $* " in *" -c "*|*" --clear "*) exit 0 ;; esac
    text="$(wl_copy_args "$@")"
    [ -n "$text" ] || text="$(cat)"
    emit_copy "$text"
    ;;
  *)
    defer "$@" ;;
esac
