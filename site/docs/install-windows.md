---
title: Install on Windows
nav: Install — Windows
description: Connect a Windows machine with one PowerShell command — what it does, the Node.js prerequisite, and how to update.
---

# Install on Windows

One PowerShell command connects a Windows machine to your account (replace
`<machine-name>`, e.g. `my-desktop`):

```powershell
irm "https://app.ccscreen.dev/install.ps1?name=<machine-name>&assistants=all" | iex
```

The [dashboard](https://app.ccscreen.dev)'s **Add a machine** card (Windows
tab) generates this command with your chosen name filled in. The name and the
assistant choice ride the URL's query string because `irm … | iex` can't take
arguments the way `sh -s --` can; leave `?name=` off and the machine is named
after `$env:COMPUTERNAME`.

Run it in a regular PowerShell window — **no administrator prompt needed**.
Everything installs for your user only.

## What it does

1. **Installs the binary.** `cc-screen-rust.exe` lands in
   `~\.local\bin` and is added to your user PATH. If a previous install is
   running, the script stops it first (Windows locks a running `.exe`), so
   re-running the one-liner is also the update path.
2. **Checks / installs the coding assistants.** With `assistants=all` every
   missing CLI among Claude, Codex, Gemini, and Kimi is installed under your
   user profile; use `assistants=claude,codex` to narrow it, or drop the
   parameter to just get a report. Two Windows-specific caveats:
   - **Codex and Gemini need Node.js**, which on Windows is a machine-wide
     installer. If `npm` isn't present, those rows report it with a link
     rather than guessing — install Node once from
     [nodejs.org](https://nodejs.org/en/download) and re-run.
   - **Kimi is unverified on Windows.**
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
- If you'd rather pass the machine name as an argument (no query string):

  ```powershell
  & ([scriptblock]::Create((irm https://app.ccscreen.dev/install.ps1))) my-windows-box
  ```
