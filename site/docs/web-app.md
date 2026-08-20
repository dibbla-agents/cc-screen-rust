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

**Recent** sits at the top of the list: up to ten of the sessions you were
last *working in*, most recent first, lifted out of their machine groups. It
answers "where was I two sessions ago?", so switching back and forth between
the two or three you're actively driving needs no reading — the order only
changes when *you* focus a session, never when an agent goes ready or starts
talking. The session you're currently in isn't listed (it's already on
screen), and when the drawer opens the cursor is already parked on the top
Recent row: on a laptop `⌃B` `⏎` takes you back to the last session, and on a
phone it's the first row under your thumb.

This list lives in *this* browser — it isn't synced between devices, and
`ccs` on the same machine keeps its own (see the [FAQ](../faq/)). Clearing
your browser storage clears it; nothing else is affected.

**New session** starts one: pick the machine, the tool (Claude, Codex,
Gemini, Kimi, or a plain shell), and the working directory (with a
directory search, so deep project paths are a few keystrokes).

The panel opens aimed at **the machine you last used a session on** (from
this browser), and the machine list is in that same most-recently-used
order — so the common case needs no machine click at all, and the uncommon
one finds its machine near the top. On a single-machine setup there's no
picker to see.

On a laptop the whole thing is two keystrokes: **`⌃B` `n`** opens the create
panel directly, empty, with the folder box focused — type a few letters of
the project and press `⏎`. (Typing `new` into the session list to find the
button works too, and the panel still opens empty: the word that summoned
the button isn't a folder. Type `new myproj` and the `myproj` half *does*
carry into the folder search.)

Two details worth knowing:

![Creating a session — tool, directory, options](../img/web-new-session.png)

- **Skip permissions** (on by default) launches the CLI with its
  approval-bypass flag — the agent won't stop to ask before running tools.
  Turn it off for a session you want to supervise; sessions with it off wear
  a "safe" badge.
- **Claude app** (off by default) turns on Claude Code's *own* remote
  control, registering the session with claude.ai/code and the Claude mobile
  app under the name `claude-<session>`. Leave it off and the session stays
  inside cc-screen — one remote-access layer per session, which is the point.
  Sessions with it on wear a "claude app" badge. The switch only appears for
  tools that have such a feature (today: Claude Code).
- Sessions survive you. Closing the browser, losing signal, even the machine
  rebooting — the agent resumes its sessions and your panes re-attach.

### Restart a session

Every assistant row has a **restart** button (the circular arrow, beside the
trash). Press it once to arm the confirm, once more to fire: cc-screen types
the assistant's `/exit`, waits for it to quit, and relaunches it in place with
its conversation resumed. On desktop the same button sits in the focused
pane's header bar, so you don't have to open the drawer for the session you're
already looking at.

It's the button for **anything the CLI only reads at launch** — a newly added
MCP server is the usual one. Nothing else on the machine changes, and no other
session is touched.

What survives: the session's name, its pane (the pane blinks, it doesn't
disappear — the name never changes, so it re-attaches by itself), its colour
mark and label, its folder and extra folders, and its **Skip permissions** and
**Claude app** settings.

What resume actually means is worth knowing, because it isn't magic:

- The conversation is resumed the same way a reboot resumes it — `--continue`
  for Claude Code and Kimi, `resume --last` for Codex, `--resume latest` for
  Gemini. If there's nothing resumable, the session comes back **empty rather
  than dead**.
- `--continue` picks the most recent conversation **in the folder the session
  launched in**. Two cc-screen sessions in the *same* folder therefore both
  continue whichever of them wrote last — so restarting one can resume the
  other's conversation. It's inherent to this kind of resume (the reboot path
  and the dashboard's Update have it too), not something the button adds.
- The relaunch uses the session's **launch folder**, not wherever you last
  `cd`'d inside it.
- A turn in flight loses its last exchange. If the agent doesn't look ready,
  the confirm says so — but it never blocks you, because "it's wedged" is a
  perfectly good reason to restart.

Restart isn't offered for plain **shell** sessions: there's no conversation to
resume, so restarting one would just throw your shell state away.

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
palette to switch arrangements (**`⌃B` `l`**), and each pane is a full live
terminal. The whole grid is drivable from the keyboard — see
[Keyboard shortcuts](#keyboard-shortcuts) for the pane chords.

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

## Copying text out of a session

Three ways, depending on who is doing the copying.

**You select, you copy.** Drag across terminal text and press **⌘C** (Mac) or
**Ctrl+C** (Linux/Windows). With no selection, Ctrl+C still goes through to the
session as an interrupt — that never changed.

**When a full-screen app has taken the terminal**, a plain drag is a *mouse
gesture that belongs to the app* (Claude Code's full-screen renderer, `htop`,
`lazygit` and friends all ask for the mouse), so it selects nothing. Hold
**⌥ Option** on a Mac browser, **Shift** everywhere else, and drag — or click
**Select text** in the corner of the pane, which lets a plain drag select until
you turn it off. The pane tells you which modifier applies; it is the
*browser's* platform that decides, not the machine the session runs on.

**The assistant copies.** When something inside the session copies — Claude
Code's `/copy`, its copy-on-select, `Ctrl+C` on a selection inside its own UI,
`/export → Clipboard` — the text now lands on **your** clipboard, on the device
you're holding. It used to be thrown away, or (on a Mac agent) land on the
*agent machine's* clipboard while the assistant told you it had worked.

A few rules keep that from being a way for a session to write your clipboard
whenever it likes:

- Only the session you are **actively driving** — focused pane, focused tab,
  and you've typed into it recently — gets a silent write. A session you are
  merely *watching*, including a teammate's, never writes your clipboard on its
  own; you get a **Copy** button instead, showing exactly what it would copy.
- Multi-line text, and anything that had control characters or
  direction-override characters removed, always takes the button, never the
  silent path.
- Very large copies (over 64 KB) are announced, not copied.
- A session that copies over and over is rate-limited and then switched off,
  with a notice you can re-enable.
- The reverse direction — a session **reading** your clipboard — is not
  implemented and won't be. That request is ignored, always.

Scrolling behaves the same way, split by who owns the screen: on a phone, a
swipe scrolls the terminal's own scrollback normally, and scrolls the
**application's** view when a full-screen app has taken over. On the desktop
the mouse wheel does the same — the app's own view while it's running, the
browser's scrollback outside it.

## Keyboard shortcuts

On a laptop, cc-screen drives the whole grid from one prefix key: **`⌃B`**
(Ctrl+B), then a second key. It's the tmux idea, and it's the only keyboard
namespace the app claims — `Cmd/Ctrl+F` and the rest of your browser's keys are
never taken. The prefix is **desktop-only**: it needs a real pointer and a
window at least 900 px wide, so on a phone (or a narrow window) there's one
pane and no chords. And it stays out of the way of typing: whenever the caret is
in a text field — the compose box, a search box, a rename field — `⌃B` goes to
that field instead.

**The prefix itself**

- **`⌃B`** arms the prefix. Press the second key within 600 ms.
- **`⌃B` held for ~½ a second**, with no second key, opens the session list.
- **`⌃B` `Esc`** cancels an armed prefix.

**Moving between panes**

- **`⌃B` `←` / `→`** move the focus between panes, wrapping around. After one,
  you get about 800 ms in which bare `←` / `→` keep stepping — so you can walk
  the grid without re-pressing the prefix.
- **`⌃B` `1`–`9`** jump straight to a pane. The number is the small digit in the
  corner of each pane, shown whether or not that pane holds a session.
- **`⌃B` `;`** goes back to the pane you came from, and pressing it again
  returns — for bouncing between the two sessions you're actually working in.
- **`⌃B` `l`** (or **`⌃B` `Space`**) opens the layout palette.
- **`⌃B` `x`** clears the focused pane — the session keeps running.

**The session in a pane**

- **`⌃B` `↑` / `↓`** change *which session* the focused pane shows. This is the
  one to keep straight: left/right moves between panes, up/down changes what's
  inside one. (On an *empty* pane the bare arrows belong to its session
  switcher, so there you press the prefix each time.)
- **`⌃B` `s`** opens the session list.
- **`⌃B` `n`** starts a new session: the create panel, empty, on the machine
  you last used. The session lands in the focused pane.
- **`⌃B` `r`** renames the focused session (same as double-clicking its name).
- **`⌃B` `c`** re-rolls the focused session's colour mark; **`⌃B` `⇧C`** clears
  it.
- **`⌃B` `⇧S`** shares the focused session with a teammate — team accounts on
  ccscreen.dev only; elsewhere it does nothing.

**Files and search**

- **`⌃B` `/`** finds text in the focused terminal (see above).
- **`⌃B` `e`** opens the file browser / editor for the focused session.
- **`⌃B` `f`** opens it and jumps straight to find-a-file.
- **`⌃B` `t`** opens it and jumps to the tree filter.
- **`⌃B` `l`** copies a link to the file the editor has open (see
  [Link to a file](#link-to-a-file)).

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

### Tables

Markdown tables render as real tables while you write, and stay that way while
you edit them:

- **Click a cell** and the caret lands *in that cell's text* — not at the top of
  the table.
- Only the **row you're editing** shows its raw `| … |` source; every other row
  keeps rendering, so a thirty-row table never turns into pipe soup because you
  fixed one number.
- **Tab / Shift-Tab** walk cell to cell (and on to the next row) while you're on
  a table row. Everywhere else Tab is untouched.
- **Copy** (top-right of the table) puts the table on the clipboard as markdown
  — your own bytes, exactly as written — ready to paste into a chat, an issue,
  or another file.
- Pasting a block copied from **Excel, Google Sheets, Numbers** or any web page's
  table turns it into a well-formed, aligned markdown table. If that isn't what
  you wanted, one **⌘Z / Ctrl-Z** puts the plain text back.

### Link to a file

Every file has a URL, and the address bar shows it while the file is open —
so **bookmarking a file is just ⌘D / ⭐**, and the bookmark opens it again in
one tap, on desktop and on the phone alike:

```
https://app.ccscreen.dev/file/studio/projects/planning/tasks.md
```

The URL names the **machine** and the path **relative to that machine's home
folder** — a path only means something on the machine whose tree produced it,
so a link is never rewritten for someone else's box. A trailing `/` makes it a
folder link, which opens the tree there instead of a file.

To get the URL without visiting the file: **Copy link** in the file tree's
right-click / long-press menu (files *and* folders), the 🔗 button in the
editor's toolbar, or **`⌃B` `l`** while the editor is open.

Two things worth knowing:

- If you're signed out, the link takes you through sign-in and then opens the
  file — you don't lose your place.
- If the file has moved or been deleted, the link doesn't break in your face:
  it drops you in the file tree with a one-line note. If the machine is simply
  asleep, it says so and offers a retry.

A `/file/…` link is *not* a permission — it's an address. It opens as **you**,
with the access you already have. To let someone else read a file, see
[Share a read-only link](#share-a-read-only-link).

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
- Either way they get the machine's **files** too — the tree, the editor,
  download and upload — even when you shared a single session. They can already
  ask the assistant in that terminal to print or change anything it can reach,
  so the file browser gives them nothing new; see
  [Security](../security/#teams) before sharing.

Either way cc-screen emails the invitation, and either way you also get a
copyable invite link. Keep that link handy: it's the same invitation, and
it's the fallback for a spam folder, a typo'd address, or a self-hosted hub
whose operator hasn't configured a mail relay (those send no mail at all).

You can see everything shared by you and with you on the dashboard's
**Sharing** card, and revoke (or leave) any of it there. Access stops
immediately on revoke.

### Share a read-only link

Sometimes the person you want to show a file to doesn't have an account and
shouldn't need one. **Share read-only link…** — in the file tree's
right-click / long-press menu, or the editor's ⋯ menu — mints a URL like:

```
https://app.ccscreen.dev/s/8Kq2…
```

Anyone holding that URL sees **that one file**, rendered and read-only. No
account, no sign-in, and nothing else on your machine is reachable through it:
not the folder it sits in, not the session, not the terminal. It cannot be
edited through the link — editing means signing in.

- **The URL is shown once.** Only a fingerprint of it is stored, so nobody
  (including us) can show it to you again. Lost it? **New URL** on the row
  mints a replacement and kills the old one at that instant.
- **Revoke any time** from the **Sharing** card. Access stops immediately,
  including for anyone who has the page open.
- It serves text files (markdown renders as a page; code is
  syntax-highlighted). PDFs, images and other binaries get a "no preview"
  page rather than downloading.
- A rename breaks the link on purpose — a grant is never silently re-pointed
  at a different file.

See [Security](../security/#read-only-link-grants) for exactly what a link
holder can and cannot learn.

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
  That's the machine-wide one: it restarts *every* session using an updated
  CLI. To restart just one session (without updating anything), use the
  restart button on its row — see
  [Restart a session](#restart-a-session).
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
