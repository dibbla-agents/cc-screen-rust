---
title: Install on Windows
nav: Install — Windows
description: Connect a Windows machine with one PowerShell command — what it does, the Node.js prerequisite, and how to update.
casts: true
---

# Install on Windows

One PowerShell command connects a Windows machine to your account (replace
`<machine-name>`, e.g. `my-desktop`):

```powershell
irm "https://app.ccscreen.dev/install.ps1?name=<machine-name>&assistants=all" | iex
```

A real run, end to end (~10 s):

<pre class="cast" data-cast="../media/install-windows.cast">
  ✓ claude     (Claude Code)  C:\Users\erik\.local\bin\claude.EXE
  ✓ gemini     (Gemini CLI)  C:\Users\erik\AppData\Roaming\npm\gemini
  ✓ codex      (Codex CLI)  C:\Users\erik\AppData\Roaming\npm\codex

cc-screen-rust running in --hub-only mode (no local port; reachable via the hub)

OK — 'harebell' is connected and will reconnect automatically.
</pre>

The [dashboard](https://app.ccscreen.dev)'s **Add a machine** card (Windows
tab) generates this command with your chosen name filled in. The name and the
assistant choice ride the URL's query string because `irm … | iex` can't take
arguments the way `sh -s --` can; leave `?name=` off and the machine is named
after `$env:COMPUTERNAME`.

![The Add a machine card with the Windows tab selected](../img/web-add-machine.png "The Add a machine card (Windows tab): name the box, pick assistants, copy the PowerShell one-liner.")

Run it in a regular PowerShell window — **no administrator prompt needed**.
Everything installs for your user only.

## What it does

1. **Installs the binary.** `cc-screen-rust.exe` lands in
   `~\.local\bin` and is added to your user PATH. If a previous install is
   running, the script stops it first (Windows locks a running `.exe`), so
   re-running the one-liner is also the update path.
2. **Checks / installs the coding assistants.** With `assistants=all` every
   missing CLI among Claude, Codex, Gemini, Kimi, OpenCode, and Grok is installed under your
   user profile; use `assistants=claude,opencode` to narrow it, or drop the
   parameter to just get a report. Two Windows-specific caveats:
   - **Codex, Gemini, and OpenCode need Node.js**, which on Windows is a machine-wide
     installer. If `npm` isn't present, those rows report it with a link
     rather than guessing — install Node once from
     [nodejs.org](https://nodejs.org/en/download) and re-run.
   - **OpenCode installs natively through `npm install -g opencode-ai`, but its
     native-Windows launch/update/resume flow is not yet a claimed release gate.**
     OpenCode recommends **WSL** for the best Windows experience; use that route
     for production work until the native pass is recorded.
   - **Kimi is unverified on Windows.**
   - **Grok uses the official PowerShell installer** (`irm https://x.ai/cli/install.ps1 | iex`)
     and does **not** need Node. The installer prepends User PATH; cc-screen
     restores it and copies `grok.exe` into `%USERPROFILE%\.local\bin`. Native
     Windows support is claimed only after the `harebell` install/launch/update/
     exit/resume pass.
3. **Enrolls the machine.** A short code prints (like `WDJB-MJHT`); approve it
   at [app.ccscreen.dev/activate](https://app.ccscreen.dev/activate) from any
   logged-in browser. Codes expire after 10 minutes, and the installer prints
   a fresh one automatically if that happens.
4. **Registers a Task Scheduler task** that starts the agent at logon, in
   hub-only mode (no inbound port — the agent only dials out to the hub).

## Updating

Re-run the same one-liner — it stops the running agent, swaps the binary, and
re-registers the task; the machine keeps its identity (no new code to
approve). Assistants update from the dashboard's per-machine **Update**
button, or on the box with `cc-screen-rust doctor --update`.

## Uninstalling

```powershell
cc-screen-rust uninstall
```

removes the scheduled task and the binary. **Unlink** the machine from its
dashboard row to detach it from your account.

## Notes

- The session shell on Windows is PowerShell; sessions and the file
  browser/editor work the same as on macOS/Linux.
- To *drive* sessions from this box as well — the whole fleet in a terminal
  switcher and grid — the `ccs` client is one more command:

  ```powershell
  powershell -ExecutionPolicy Bypass -Command "irm https://app.ccscreen.dev/ccs.ps1 | iex"
  ```

  See [the ccs terminal client](../tui/#install) for signing it in and the
  keys. It's independent of the agent above — install it on any Windows box
  you work from, connected machine or not.
- If you'd rather pass the machine name as an argument (no query string):

  ```powershell
  & ([scriptblock]::Create((irm https://app.ccscreen.dev/install.ps1))) my-windows-box
  ```
