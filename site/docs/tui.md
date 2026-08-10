---
title: The ccs terminal client
nav: The ccs terminal client
description: ccs — a native terminal client with a session switcher and a multi-machine grid, for people who live in a terminal.
---

# The ccs terminal client

`ccs` is a native terminal app that speaks to the same hub (or to a single
machine's agent directly, if you self-host without a hub). A session
switcher, a multi-pane grid with real scrollback, tmux-style keys — your
whole fleet without leaving the terminal.

Why not just `tmux`? tmux attaches you to one machine you can already reach;
`ccs` attaches you to *every* machine your hub can — over a single
outbound-only uplink, with agent-aware ready notifications — no inbound SSH
and no per-machine tmux sockets.

![The ccs grid — three sessions from the demo fleet in the left-L layout](../img/tui-grid.png)

## Install

**macOS and Linux**

```sh
curl -fsSL https://app.ccscreen.dev/ccs.sh | sh
```

drops `ccs` into `~/.local/bin` and prints the sign-in command. (The generic
installer, if you'd rather not fetch through the hub:
`curl --proto '=https' --tlsv1.2 -LsSf
https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-tui-installer.sh | sh`.)

**Windows**

```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://app.ccscreen.dev/ccs.ps1 | iex"
```

drops `ccs.exe` into `~\.local\bin` and adds that to your user PATH — open a
new terminal afterwards and `ccs` is there. No administrator prompt; it
installs for your user only. The `-ExecutionPolicy Bypass` prefix applies to
that one PowerShell process and changes nothing on the machine: a default
Windows desktop blocks scripts outright, and without the prefix the installer
stops and says so. Run `ccs` from **Windows Terminal** — the old `conhost`
console can't draw it.

It's a real terminal app — it needs an interactive TTY, on every platform.

## Connect (hosted — app.ccscreen.dev)

```sh
ccs activate
```

(or just `ccs` — with no sign-in yet it runs the activation itself, then
drops you straight into the switcher). It prints a short one-time code and a
URL, polls for your approval, and confirms the account it signed in as:

![ccs activate: the one-time code, the URL, and the signed-in confirmation](../img/ccs-activate.png)

Open the URL from any logged-in browser — your phone is fine, and it works
over SSH because nothing has to open on the box itself — type the code, and
approve. That's it: `✓ Logged in as you@example.com`, and plain `ccs` from
then on connects as you. Codes expire after 10 minutes; a fresh one is a
re-run away. It's the same code-approve gesture as enrolling a machine, just
signing in a *terminal* instead.

![Approving the terminal's code on the /activate page](../img/web-approve-terminal.png)

`ccs logout` signs the terminal out again — it revokes the credential
server-side and forgets it locally. You can also revoke any terminal from
the dashboard's **Terminal clients** list.

## Connect (self-hosted)

Point it at your own endpoint with a static token:

```sh
ccs --server http://laptop:8839          # one machine's agent, directly
ccs --server http://hub:8840 --token …   # your own hub (all machines)
```

The server URL and token are remembered, so from then on plain `ccs`
reconnects. This is the *client* token — the credential a browser or script
uses to talk to the hub (`CCWEB_API_TOKEN` on the server / `--token` on
`cc-screen-hub install`) — not a machine's uplink credential. A self-hosted
*multi-tenant* hub supports `ccs activate` too.

(`--insecure` accepts a self-signed certificate for an ad-hoc `wss` setup.)

## Credentials

- Device sign-ins land in `~/.config/cc-screen-tui/credentials.toml` —
  owner-only (`0600`), keyed per hub, never printed. The server URL lives in
  `config.toml` next to it.
- Precedence when several sources exist: `--token` > `CCS_API_TOKEN` >
  `CCWEB_API_TOKEN` > `credentials.toml` > `api_token` in `config.toml`.
- Headless/CI use: set `CCS_API_TOKEN` (or pass `--token`) and skip
  `activate` entirely.

## Using it

`ccs` opens in the **switcher**: every machine's sessions in one list, each
with its colour, tool, and latest one-line summary, plus create / kill /
rename / restore actions. Pick sessions into the **grid** — the same six
layouts as the web app, and panes from different machines can sit side by
side, each titled `machine/name`. Each pane is a full terminal emulator with
multi-thousand-line scrollback.

![The ccs switcher — every session across the fleet in one list](../img/tui-switcher.png "The switcher: session labels, colours, tools, and each one's latest line")

Jump straight to a session — handy from a script or muscle memory:

```sh
ccs alpha            # attach directly (exact name, machine/name, prefix, or fuzzy)
```

### Search

The switcher is **search-first**, like the web sidebar: just start typing and
the list filters and re-ranks live. Matching is fuzzy and spans the session
name, its folder, its AI summary, its tool, and its machine — ranked
name > path > summary — so finding a session in a big fleet is "type 3
letters, Enter". `Esc` clears the query first; a second `Esc` quits
(`Ctrl-U` clears in one stroke).

On a hub with several machines the resting list groups sessions under
per-machine hostname headers (offline machines are marked); while you're
typing the list flattens to ranked order and each row shows its machine
inline. The selected session's full 2–3 sentence AI summary appears above the
status bar.

The grid's **action menu** (`Ctrl-A d`) works the same way: start typing and
the menu filters and re-ranks live — sessions rank exactly like the switcher,
and the action rows match on aliases too (type "split" for the layout picker,
"detach" to clear the box, "exit" to quit), with a session name hit always
winning over an action. The same rules apply: arrows move, `Enter` selects,
`Esc` clears the query first and closes second, and multi-machine hubs show
the same hostname headers at rest and machine chips while filtering. Putting
a session into a box is "`Ctrl-A d`, type 3 letters, Enter".

Because typing searches, the switcher's commands live on Ctrl-chords:

- `↑`/`↓` move · `Enter` attach the selected (top) match
- `Ctrl-N` new session · `Ctrl-X` kill · `Ctrl-E` graceful exit
- `Ctrl-R` rename the selected session
- `Ctrl-O` open the **restorable-session picker** (bring sessions back after
  a redeploy)
- `Esc` clear the search, then quit · `Ctrl-C` quit

Inside the grid the prefix key is **Ctrl-A** (tmux-style):

- `Ctrl-A d` — open the action menu (attach / rename / clear / layout) —
  search-first, see above
- `Ctrl-A s` — open the full-screen switcher to pick a session for the
  focused box
- `Ctrl-A` then a digit — switch layout
- `Ctrl-A g` — jump to a session that just went ready
- `Ctrl-A [` — enter **scrollback** mode on the focused pane (`PgUp`/`PgDn`,
  `k`/`j`, `g`/`G` top/live, `q`/`Esc` back to live); any printable key in
  normal mode snaps back to live
- click or move spatially to focus panes

![The visual layout palette open over the grid](../img/tui-layouts.png "Ctrl-A l opens the layout palette — six layouts, applied live")

Everything else you type goes straight to the focused session's PTY.

### Scrolling

What the wheel does in a pane depends on what the app inside it is doing:

- **A plain shell, or Claude Code's classic renderer** — the wheel scrolls
  the pane's own multi-thousand-line scrollback, as it always has.
- **An app that takes the mouse** (Claude Code's fullscreen renderer, `htop`,
  `lazygit`, `vim` with mouse on) — the wheel is forwarded to the app as a
  mouse report, so the *app's* own view scrolls. The same thing tmux does.
- **A full-screen app that doesn't take the mouse** (`less`, `vim` without
  mouse) — the wheel becomes three `Up`/`Down` key presses, so the file
  moves.

One exception: once you've scrolled a pane back into its own history, the
wheel keeps scrolling that history until you're back at the live bottom.

`Ctrl-A [` (scrollback mode) is unchanged on the normal screen. A full-screen
app has no scrollback to page through — the alternate screen keeps none by
design — so there `Ctrl-A [` declines and flashes `alt screen: no scrollback
(app controls its own view)` in the status bar rather than swallowing your
keys: `PgUp`/`PgDn` and `j`/`k` go straight to the app, which handles them
itself. The status bar shows a `⛶` marker while the focused pane's app is
full-screen.

Attaching to a session that's *already* running a full-screen app keeps the
scrollback from before that app started — quit the app and the earlier output
is still there, and still scrollable.

## Maintenance

```sh
ccs update       # fetch the latest ccs build
ccs logout       # sign this terminal out (revokes its credential)
ccs uninstall    # remove the ccs binary + config
ccs --help       # the full flag reference
```

All four work on Windows as well (from ccs 0.5.5 — on 0.5.4 the two that
touch the binary fail, so re-run the install command to update). One visible
difference: `ccs uninstall` finishes a moment *after* the program exits.
Windows won't let a running program delete itself, so that last step is left
to a helper that waits for it.
