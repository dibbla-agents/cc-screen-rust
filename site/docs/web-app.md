---
title: Using the web app
nav: The web app
description: Sessions, the multi-pane grid, the file browser and editor, uploads, notifications, and sharing — a tour of the cc-screen web app.
---

# Using the web app

Everything at [app.ccscreen.dev](https://app.ccscreen.dev) works in any
browser, phone or desktop. On a phone, use **Add to Home Screen** — cc-screen
is a PWA, so you get a real app icon, full-screen terminals, and push
notifications.

## Sessions and the switcher

The session drawer lists every session on every connected machine, grouped by
machine, with a live preview line so you can tell at a glance who's working
and who's waiting for you. Tap a session to attach; the terminal is the real
PTY — what you see is exactly what's running on the box, and it keeps running
when you close the tab.

**New session** starts one: pick the machine, the tool (Claude, Codex,
Gemini, Kimi, or a plain shell), and the working directory (with a
directory search, so deep project paths are a few keystrokes). Two details
worth knowing:

![Creating a session — tool, directory, options](../img/web-new-session.png)

- **Skip permissions** (on by default) launches the CLI with its
  approval-bypass flag — the agent won't stop to ask before running tools.
  Turn it off for a session you want to supervise; sessions with it off wear
  a "safe" badge.
- Sessions survive you. Closing the browser, losing signal, even the machine
  rebooting — the agent resumes its sessions and your panes re-attach.

Here's the whole flow in one take — new session, directory search, Claude
booting:

<figure>
<video controls muted playsinline preload="none" poster="../media/web-create-session-poster.png" width="1280" height="720"><source src="../media/web-create-session.webm" type="video/webm" /><source src="../media/web-create-session.mp4" type="video/mp4" /></video>
<figcaption>From "New session" to a running agent in under ten seconds.</figcaption>
</figure>

## The grid (desktop)

On a wide screen you can tile up to six sessions side by side, in six layout
templates (columns, rows, main-plus-stack, 2×2, and more). Panes can come
from *different machines* — one grid, your whole fleet. There's a layout
palette to switch arrangements, and each pane is a full live terminal.

![Three live panes with the layout palette open](../img/web-grid.png "Three sessions tiled in the main-plus-stack layout — two Claude agents and a shell — with the layout palette open.")

### Searching terminal output

`Ctrl+B /` opens a find bar over the focused pane: type to search that
session's scrollback, **Enter** / **Shift+Enter** for next / previous,
**Esc** to close and hand the keyboard back to the terminal. On a phone, the
magnifier button in the header does the same thing. In the file editor, the
agent column's magnifier searches the mirrored session.

Use this instead of the browser's own Find for terminal output: cc-screen
draws terminals on the GPU, so `Cmd/Ctrl+F` — which still works for the rest
of the page — can't see the characters. And a detail people notice: the
terminal cursor doesn't blink. That's deliberate; a blinking caret repaints
the page forever and costs real battery, and it was never part of what the
remote agent sends.

## Files: browse, edit, cowork

Every machine gets a built-in file browser and editor, confined to that
machine's home directory. The tree is live (it follows filesystem changes as
your agent works), and the editor previews markdown — handy for reading the
plan or review docs agents produce.

![Coworking on a file next to a live session](../img/web-cowork.png)

![The editor on a phone](../img/mobile-editor.png)

- **Uploads:** drop files (up to 500 MiB) onto a machine from the browser.
- **Clipboard images:** paste an image (Ctrl-V) straight into a Claude or
  Codex session — it routes to the active session on whichever machine owns
  it. cc-screen stages the image on that machine and delivers it the way that
  CLI expects: Claude reads it as a local clipboard paste; Codex gets the
  staged file attached directly (the file is kept machine-locally until the
  session is deleted, so it survives drafts and resumes). No X11 or desktop
  environment is needed on the machine.
- **Downloads:** pull any file back out, PDFs render in-app.

## Notifications

Enable notifications (the bell button) and your phone buzzes when an agent
**finishes its turn** — the moment it stops working and waits for your input.
One subscription covers every machine; the machine's name is in the title. On
iOS this requires the PWA installed to the home screen.

![The notification bell in the switcher header](../img/mobile-notifications.png "The bell lives in the session switcher's header — tap it to enable push for this device (it confirms with a test buzz).")

## Sharing

Share a whole machine (from its dashboard row) or a single session (from its
menu) with another person, as **can view** or **can use**:

- If they already have an account, the share lands in their inbox (the bell
  icon) to accept or decline.
- If they don't, the invitation is what gets them in — the share attaches
  automatically when they sign up with that email.

Either way cc-screen emails the invitation, and either way you also get a
copyable invite link. Keep that link handy: it's the same invitation, and
it's the fallback for a spam folder, a typo'd address, or a self-hosted hub
whose operator hasn't configured a mail relay (those send no mail at all).

You can see everything shared by you and with you on the dashboard's
**Sharing** card, and revoke (or leave) any of it there. Access stops
immediately on revoke.

### Teams

One-off shares scale badly past two people — a **team** replaces them with
one standing arrangement: everyone on the team automatically sees everyone
else's machines and sessions, **view-only**. No per-machine invites, no
grants to keep in sync when someone gets a new laptop.

Joining is the consent, and the invite says so before you accept, verbatim:

> Joining makes your machines on cc-screen visible to this team (view-only).
> You can hide any machine in settings.

The fine print, in plain words:

- **View, not use.** Teammates can open your sessions and watch, but
  creating sessions on your machine — or any admin action on it — still
  requires an explicit "can use" share from you, exactly as before. (Honest
  caveat: an *open* terminal accepts keystrokes — view-only bounds what a
  teammate can reach, not what a focused terminal does. See the
  [Security model](../security/#teams).)
- **Hide any machine.** Every machine of yours has a **Visible to team**
  toggle in the team window — flip it off and that machine drops out of the
  team's view immediately. The toggle belongs to the machine's owner alone,
  whatever their role, and every flip is recorded in the team's audit log.
- **Roles.** One **owner** (billing, roles, everything), any number of
  **admins** (invite, remove members), and **members**. Ownership transfers
  explicitly — the owner leaves only after handing the team over.
- **Pooled limits.** A team shares one pool: 10 machines and 50 concurrent
  sessions *per seat*, counted across the whole team rather than per person.
  Every seat gets everything Pro has; "pooled" means a heavy teammate can
  use headroom a light one doesn't.
- **Leaving cuts both ways.** Leave (or be removed) and you stop seeing the
  team's machines *and* they stop seeing yours, instantly. Personal shares
  you set up separately survive.

## The dashboard

The machines dashboard (your account page) is the admin surface:

- **online/offline** per machine, live.
- **⚠ N missing · Install** — the machine lacks some coding assistants;
  installs them remotely, for that machine's user, no sudo.
- **Update** — update a machine's assistants, then restart their sessions.
- **Share** — invite someone to the machine.
- **Rotate** — mint a new machine credential (the old one stops working;
  shown once).
- **Unlink** — detach the machine from your account; it would need to
  re-enroll to come back.
- **Add a machine** — generates the install one-liner for macOS/Linux or
  Windows. See the [Quickstart](../quickstart/).

![The machines dashboard](../img/web-machines.png "The dashboard: one machine online with '3 missing · Install' waiting, one offline, and the plan card below.")

On a team, the dashboard grows a **`~/team` window** — the team's admin
surface:

- the **member list** with each person's role; owners and admins invite by
  email (cc-screen mails the invitation and hands you a copyable link as
  well; each pending row shows how delivery went — *sending*, *sent*,
  *failed* with a **Resend**, or *bad address*) and remove members; the owner
  changes roles and can transfer ownership.
- your machines' **Visible to team** toggles (yours to flip, whatever your
  role).
- the **seats meter** on the plan card — `members / seats` — and, for the
  owner or an admin, the seat checkout and the billing portal where seat
  counts change.
- the **audit log** (owners and admins): who joined, who invited whom, every
  visibility flip, seat changes — the team's history, newest first.

If you hit your plan's machine or session limit, the app tells you which cap
you hit and how to request more — see the [FAQ](../faq/) on plans.
