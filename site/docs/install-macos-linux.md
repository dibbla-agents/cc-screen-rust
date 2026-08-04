---
title: Install on macOS / Linux
nav: Install — macOS / Linux
description: What the one-line installer does on macOS and Linux, step by step — and how to update, re-run, and uninstall.
---

# Install on macOS / Linux

One command connects a machine to your account (replace `<machine-name>`,
e.g. `my-laptop`):

```sh
curl -fsSL https://app.ccscreen.dev/install.sh | sh -s -- <machine-name> --assistants
```

If you skip the name, the machine is named after its hostname. The
[dashboard](https://app.ccscreen.dev) generates this exact command on the
**Add a machine** card, so you rarely type it by hand.

This page explains what that script actually does, in the order it does it.

## 1. The binary

The script downloads the latest `cc-screen-rust` release for your platform
(macOS arm64/x64 and Linux are auto-detected) and installs it to
`~/.local/bin/cc-screen-rust`. No sudo, no system package manager.

## 2. The coding assistants

cc-screen *drives* the coding CLIs — it doesn't bundle them — so the installer
checks which of `claude`, `codex`, `gemini`, and `kimi` this machine has:

- **`--assistants`** — install every missing one, non-interactively, for your
  user only. Everything lands under `$HOME/.local`; there is no sudo anywhere
  in the path. Budget several hundred MB.
- **`--assistants=claude,codex`** — install just those.
- **`--no-assistants`** (or no flag) — report only: each missing CLI is
  listed with the command that would install it, and nothing is installed.

A missing assistant never aborts the install — a machine with only `claude`
is a perfectly good machine for Claude sessions. You can install the rest
later from the dashboard (the machine row shows *"N missing · Install"*) or on
the box:

```sh
cc-screen-rust doctor                     # report what's installed, and where
cc-screen-rust doctor --install --yes     # install everything missing
```

## 3. Enrollment

The installer prints a short code (like `WDJB-MJHT`) and waits. You approve it
at [app.ccscreen.dev/activate](https://app.ccscreen.dev/activate) from any
logged-in browser. Codes live 10 minutes; an expired code is replaced with a
fresh one automatically.

On approval the machine receives its own credential, stored at
`~/.config/cc-screen-rust/enroll.json` with owner-only permissions. It's
scoped to this one machine — see [Security model](../security/).

## 4. The background service

Finally the script registers a background service — systemd `--user` on
Linux, launchd on macOS — that starts the agent now and on every boot. The
service runs in **hub-only** mode: the agent opens no local port at all, it
only dials out to the hub. Sessions you had running are resumed after a
restart.

## Updating

Re-running the one-liner is safe and is also the update path — it replaces
the binary and restarts the service; the machine keeps its identity (no new
code to approve). Or, on the box:

```sh
cc-screen-rust update           # update the agent itself
cc-screen-rust doctor --update  # update the coding assistants it drives
```

You can also update assistants per machine from the dashboard's **Update**
button, which restarts the affected sessions for you.

## Uninstalling

```sh
cc-screen-rust uninstall
```

removes the service and the binary. To also detach the machine from your
account, hit **Unlink** on its dashboard row (in either order — an unlinked
machine's credential simply stops working).
