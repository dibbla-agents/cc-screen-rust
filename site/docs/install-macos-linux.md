---
title: Install on macOS / Linux
nav: Install — macOS / Linux
description: What the one-line installer does on macOS and Linux, step by step — and how to update, re-run, and uninstall.
casts: true
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

![The Add a machine card](../img/web-add-machine.png "The dashboard's Add a machine card — name the box, pick the platform tab and the assistants, copy the generated command.")

A real run, end to end (~15 s):

<pre class="cast" data-cast="../media/install.cast">
  ✓ claude     (Claude Code)  /home/erik/.local/bin/claude
  ✓ codex      (Codex CLI)  /home/erik/.local/bin/codex
  ✓ gemini     (Gemini CLI)  /home/erik/.local/bin/gemini
  ✓ kimi       (Kimi CLI)  /home/erik/.local/bin/kimi
  ✓ opencode   (OpenCode)  /home/erik/.local/bin/opencode
  ✓ grok       (Grok)  /home/erik/.local/bin/grok

  Approved — connecting as 'my-laptop'.

✓ Done — 'my-laptop' is connected and will reconnect automatically.
</pre>

This page explains what that script actually does, in the order it does it.

## 1. The binary

The script downloads the latest `cc-screen-rust` release for your platform
(macOS arm64/x64 and Linux are auto-detected) and installs it to
`~/.local/bin/cc-screen-rust`. No sudo, no system package manager.

## 2. The coding assistants

cc-screen *drives* the coding CLIs — it doesn't bundle them — so the installer
checks which of `claude`, `codex`, `gemini`, `kimi`, `opencode`, and `grok` this machine has:

- **`--assistants`** — install every missing one, non-interactively, for your
  user only. Everything lands under `$HOME/.local`; there is no sudo anywhere
  in the path. Budget several hundred MB.
- **`--assistants=claude,opencode`** — install just those.
- **`--no-assistants`** (or no flag) — report only: each missing CLI is
  listed with the command that would install it, and nothing is installed.

The OpenCode row uses its official self-contained installer with
`--no-modify-path`. It writes `~/.opencode/bin/opencode`; cc-screen then links
that binary into `~/.local/bin`, which the running agent already puts on session
PATH. No shell rc file is edited and Node.js is not installed for OpenCode on
macOS/Linux. OpenCode's provider login is separate: use `/connect` in its TUI or
`opencode auth login` on the machine, and its credentials remain there.

The Grok row uses the official curl installer. That installer always appends a
PATH block to shell rc files and has no skip flag; cc-screen snapshots those
files, runs the vendor command, restores them byte-for-byte (and unlinks an rc
the installer created from nothing), then links `~/.grok/bin/grok` into
`~/.local/bin`. A vendor `agent` symlink in `~/.local/bin` is removed if this
run created it. First login from a phone is `grok login --device-auth` inside
the session; credentials stay in `~/.grok/`. Update with `grok update`.

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

![Approving an enrollment code on /activate](../img/web-activate.png "The /activate page: type the code, hit Approve, and the machine links to your account.")

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
