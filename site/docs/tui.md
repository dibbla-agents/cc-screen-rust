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

![Four sessions in the ccs grid](../img/tui-grid.png)

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-tui-installer.sh | sh
```

drops `ccs` into `~/.local/bin`. It's a real terminal app — it needs an
interactive TTY.

## Connect

```sh
ccs --server https://app.ccscreen.dev --token <token>
```

The server URL and token are remembered in
`~/.config/cc-screen-tui/config.toml`, so from then on plain `ccs`
reconnects. The token can also come from `api_token` in that config file or
the `CCS_API_TOKEN` / `CCWEB_API_TOKEN` environment variables.

This is the *client* token — the credential a browser or script uses to talk
to the hub — not a machine's uplink credential.

Self-hosting? Point it at your own endpoint instead:

```sh
ccs --server http://laptop:8839          # one machine's agent, directly
ccs --server http://hub:8840 --token …   # your own hub (all machines)
```

(`--insecure` accepts a self-signed certificate for an ad-hoc `wss` setup.)

## Using it

`ccs` opens in the **switcher**: every machine's sessions in one list, each
tagged with its machine and its latest one-line summary, plus create / kill /
rename / restore actions. Pick sessions into the **grid** — the same six
layouts as the web app, and panes from different machines can sit side by
side. Each pane is a full terminal emulator with multi-thousand-line
scrollback.

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

Everything else you type goes straight to the focused session's PTY.

## Maintenance

```sh
ccs update       # fetch the latest ccs build
ccs uninstall    # remove the ccs binary + config
ccs --help       # the full flag reference
```
