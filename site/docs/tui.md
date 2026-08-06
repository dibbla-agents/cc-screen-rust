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

```sh
curl -fsSL https://app.ccscreen.dev/ccs.sh | sh
```

drops `ccs` into `~/.local/bin` and prints the sign-in command. (The generic
installer, if you'd rather not fetch through the hub:
`curl --proto '=https' --tlsv1.2 -LsSf
https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-tui-installer.sh | sh`.)
It's a real terminal app — it needs an interactive TTY.

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

In the switcher:

- `↑`/`↓` or `j`/`k` move · `Enter` attach · `n` new · `x`/`e` kill · `r` refresh
- `R` — **rename** the selected session (when the list is empty, `R` opens the
  **restorable-session picker** to bring sessions back after a redeploy)

Inside the grid the prefix key is **Ctrl-A** (tmux-style):

- `Ctrl-A d` — open the action menu (attach / rename / clear / layout)
- `Ctrl-A` then a digit — switch layout
- `Ctrl-A g` — jump to a session that just went ready
- `Ctrl-A [` — enter **scrollback** mode on the focused pane (`PgUp`/`PgDn`,
  `k`/`j`, `g`/`G` top/live, `q`/`Esc` back to live); any printable key in
  normal mode snaps back to live
- click or move spatially to focus panes

![The visual layout palette open over the grid](../img/tui-layouts.png "Ctrl-A l opens the layout palette — six layouts, applied live")

Everything else you type goes straight to the focused session's PTY.

## Maintenance

```sh
ccs update       # fetch the latest ccs build
ccs logout       # sign this terminal out (revokes its credential)
ccs uninstall    # remove the ccs binary + config
ccs --help       # the full flag reference
```
