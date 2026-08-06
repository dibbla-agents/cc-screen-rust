#!/usr/bin/env bash
# The ccs terminal stills (manifest ids tui-switcher / tui-layouts / tui-grid).
#
# ccs needs a real interactive TTY, so the Playwright pipeline can't drive it.
# This script is the reproducible recipe instead: run ccs inside a scratch tmux
# session (a tmux pane IS a real TTY), drive it to the wanted screen, then
# `snap` dumps the pane with colors (capture-pane -e) and renders a crisp PNG
# via ansi2png.mjs (SGR → HTML spans, screenshotted at deviceScaleFactor 2;
# set CAPTURE_WS_ENDPOINT to use the real-Chrome browser server).
#
#   ./tui-shot.sh start [COLS ROWS]     make the tmux session (default 212x53)
#   ./tui-shot.sh snap out.png [args]   capture + render; extra args go to
#                                       ansi2png.mjs (--cols/--font-size/…)
#   ./tui-shot.sh stop                  kill the tmux session
#
# The 0061 shots, for the record (each ~1700 px wide at 2x):
#   tui-grid / tui-layouts   start 212 53 → sign in with an isolated HOME
#                            (HOME=/tmp/ccs-tui-demo-home ccs --server
#                            https://app.ccscreen.dev activate — approve the
#                            code as the demo account), attach the three demo
#                            sessions in the left-L layout (Ctrl-A 4, fill the
#                            boxes via Ctrl-A d), then:
#                              snap tui-grid.png --cols 212
#                            open the palette (Ctrl-A l) for:
#                              snap tui-layouts.png --cols 212
#   tui-switcher             start 120 14 → in ccs clear the only box
#                            (Ctrl-A d → Clear this box) to land in the
#                            full-screen switcher, then:
#                              snap tui-switcher.png --cols 120 \
#                                --font-size 11.8 --line-height 17.7
set -euo pipefail
cd "$(dirname "$0")"

SESSION="${TUI_SHOT_SESSION:-tui-shot}"
cmd="${1:-}"

case "$cmd" in
  start)
    tmux new-session -d -s "$SESSION" -x "${2:-212}" -y "${3:-53}"
    echo "tmux session '$SESSION' (${2:-212}x${3:-53}) — attach with: tmux attach -t $SESSION"
    echo "run ccs there (isolated config: HOME=/tmp/ccs-tui-demo-home)"
    ;;
  snap)
    out="${2:?usage: tui-shot.sh snap out.png [ansi2png args]}"
    shift 2
    tmux capture-pane -t "$SESSION" -e -p > out/tui-shot.ans
    node ansi2png.mjs out/tui-shot.ans "$out" "$@"
    ;;
  stop)
    tmux kill-session -t "$SESSION"
    ;;
  *)
    echo "usage: tui-shot.sh start [cols rows] | snap out.png [ansi2png args] | stop" >&2
    exit 2
    ;;
esac
