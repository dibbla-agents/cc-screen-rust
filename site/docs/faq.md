---
title: FAQ
nav: FAQ
description: Short answers — pricing, plans, supported CLIs, phones, privacy, and what happens when things restart.
---

# FAQ

## What does it cost?

The **Free** plan is free forever — 2 machines, 5 concurrent sessions, no card
required. **Pro** is $10/month (or $96/year — $8/month) and raises that
to 10 machines and 50 concurrent sessions, with unlimited sharing. **Team** is
$16/seat/month (or $160/seat/year), minimum 3 seats: every seat gets
everything Pro has, the whole team sees each other's sessions automatically,
and the limits pool — 10 machines and 50 concurrent sessions *per seat*,
shared across the team. Payments are
handled by Stripe as merchant of record (the seller is Techier AI Sthlm AB);
cancel any time from the billing portal.

## What happens when I hit a limit?

New machine approvals or new sessions are declined until you're under the cap —
the app tells you which cap and offers the upgrade. **Nothing is deleted**:
existing machines stay enrolled, running sessions keep running, your files are
never touched. Same on downgrade or a failed payment: caps apply to *new*
activity only.

## I signed up during the beta — what changes?

Nothing, unless you want it to. Beta accounts moved to the **beta** plan: the
same 10 machines / 50 concurrent sessions you had, still free, and it stays
that way. If you'd like to support the project (and get Pro), the founder price
— $5/month, locked for as long as you stay subscribed — is claimable for a
limited time while the beta pricing transition runs.

## Which coding CLIs are supported?

**Claude Code** (`claude`), **Codex** (`codex`), **Gemini CLI** (`gemini`),
**Kimi** (`kimi`), **OpenCode** (`opencode`, picker command `oc`), and **Grok**
(`grok`, picker command `gk`) — plus a
plain **shell** session, which is just your
shell on the box and always available. A machine only offers the CLIs it
actually has installed; the installer's `--assistants` flag (and the
dashboard's Install button) can install the missing ones. Self-hosters can
also declare custom tools per machine.

## Where do I connect OpenCode to a provider?

Inside OpenCode itself: use `/connect` in the TUI or run `opencode auth login` on
the machine. Provider choice, `auth.json`, configuration, and conversation data
stay under that machine user's home. cc-screen neither asks for nor copies those
credentials, and it runs the normal OpenCode TUI — not `opencode serve`.

## How do I log Grok in from a phone?

Type `grok login --device-auth` inside the session. A default `grok` launch
would open a browser on the *agent* machine, which is the wrong device when
you're on a phone. Device-code stays in the terminal; cc-screen never handles
the credential, never reads `~/.grok/auth.json`, and never starts
`grok agent serve`.

## Where does my code live? What does the hub see?

On your machines, full stop. The hub stores your account, your machine
registrations, and sharing grants — never your repositories or your CLI
logins. What passes *through* it is the relayed view: the terminal bytes and
file contents you're currently looking at. See
[Security model](../security/).

## What happens when the hub restarts?

Nothing, from your sessions' point of view. Your machines own their sessions;
the hub is a relay. Agents reconnect automatically and the session list comes
back. Your **Recent** list is local to the client, so it survives a hub
restart untouched. Same answer for your phone losing signal or your browser closing —
sessions run server-side on *your* box, not in the browser tab.

## And when the machine itself reboots?

The agent starts on boot (it's a background service) and resumes its recorded
sessions with each CLI's own resume flag — Claude Code, Kimi, OpenCode, and Grok
sessions come back via `--continue`. It's the same mechanism behind the
per-session **restart** button in the session drawer, which is how you make an
assistant re-read something it only loads at launch (an MCP server, say)
without losing the session — see
[Restart a session](../web-app/#restart-a-session).

## Can I use cc-screen without the hosted service?

Yes — fully. Run a single agent and connect to it directly, or run your own
hub (even your own multi-tenant hub with signup and enrollment). Same
binaries, no feature gap in the core product. See
[Self-hosting](../self-hosting/).

## Can I share access with a teammate?

Yes — share a whole machine or a single session, as "can view" or "can use".
cc-screen emails the invitation and hands you a copyable link as well; if the
invitee has no account yet, the share attaches when they sign up with that
email. Revoke any time from the dashboard's Sharing card. Or skip the one-off
shares and start a **team** — everyone on it sees everyone's sessions
automatically, view-only. See
[Using the web app → Sharing](../web-app/#sharing).

## Can I share a file with someone who doesn't have an account?

Yes — **Share read-only link…** on any file (right-click / long-press it in the
file tree, or the editor's ⋯ menu) mints a URL that shows that one file,
rendered and read-only, to whoever holds it. No account, no sign-in, and
nothing else on your machine is reachable through it. It can't be edited
through the link; editing means signing in.

The URL is shown once (we store only a fingerprint of it), you can mint a
replacement at any time, and you can revoke it from the dashboard's Sharing
card — access stops immediately. See
[Using the web app → Share a read-only link](../web-app/#share-a-read-only-link)
and [Security](../security/#read-only-link-grants).

## Can I bookmark a file?

Yes. The address bar tracks whatever file the editor has open, so ⌘D (or ⭐, or
"Add to Home Screen") captures it, and the bookmark reopens that file in one
tap — on the phone too, where nothing was remembered before. You can also grab
the URL without visiting the file: **Copy link** in the file tree's menu, the
🔗 button in the editor toolbar, or `⌃B` `l`.

A `/file/…` bookmark is an address, not a permission: it opens as you, with the
access you already have, and a signed-out visit goes through sign-in first and
then lands on the file. If the file has since moved, the link drops you in the
file tree with a note instead of an error. See
[Using the web app → Link to a file](../web-app/#link-to-a-file).

## How do teams work?

You join by accepting an email invite, and the invite states the deal before
you do: joining makes your machines visible to the team, **view-only** —
teammates can watch your sessions, but creating sessions on your machine (or
anything administrative) still requires an explicit "can use" share from you.
You can hide any of your machines from the team at any time (the **Visible to
team** toggle in the team window), and every flip is recorded in the team's
audit log — which the team's owner and admins can read. Leaving cuts both
ways: you stop seeing the team's machines and they stop seeing yours,
immediately. Personal shares survive team membership changes.

## How do team seats work?

Accepting an invite consumes a seat; when the team is full, the next accept
is refused until someone adds seats (the owner or an admin, from Billing) —
nothing is lost, the invite just waits. Removing a member frees their seat
instantly. Reducing the seat count never removes members: if you drop below
the current member count, the team just can't grow until it's back under.

## What's in the invite email, and what does clicking the link do?

Plain text, one message per invitation: who invited you, what to (a team, a
machine, or a single session), the team's consent sentence verbatim when it's
a team, and one link. No attachments, no reminders, and nothing else is ever
sent to that address. One caveat we'd rather not have: our mail provider
rewrites the message as HTML and adds an open-tracking pixel on the way out. We
don't use what it reports and can't turn it off on our current plan — see
[Security](../security/#invitations-and-the-mail-that-carries-them).

The link only *identifies* the invitation — it grants nothing on its own.
Accepting means signing in with the address that was invited, so a forwarded
link gets the forwarder's friend nowhere. Invites expire after 14 days, and
whoever sent it can cancel it sooner; either way the link stops working. If
the mail never turns up, ask the sender for the copyable link — it's the same
invitation, and it's the only channel on a self-hosted hub whose operator
hasn't configured a mail relay.

## Does it work on phones?

That's half the point. The web app is a PWA — **Add to Home Screen** gives
you an app icon, full-screen terminals sized to the phone, the file
browser/editor, and push notifications when an agent finishes its turn (on
iOS, notifications require the home-screen install).

## Does an open cc-screen tab drain my battery?

It shouldn't. A tab sitting there with terminals open and nothing happening
is designed to be **quiescent**: no blinking cursor, no perpetual background
animation, and terminals drawn on the GPU. Polling slows to once a minute
when the tab is hidden (just enough to keep the tab title and app badge
honest) and picks straight back up when you return.

If a tab *is* burning CPU, the usual suspects are an agent producing output
non-stop (that's real work, not waste) or an extension injecting animation
into the page. Chrome's Task Manager (Window ▸ Task Manager) shows the tab's
own CPU; an idle cc-screen tab should sit near zero.

## Why does Codex mention X11 when I paste an image?

Your agent predates assistant-aware image delivery: it asked Codex to read
the machine's X11/Wayland clipboard, which headless boxes don't have. Update
the agent on that machine and pasted images reach Codex as a staged local
file instead — no display server, no Xvfb. Details in
[Troubleshooting](../troubleshooting/#codex-says-clipboard-unavailable-an-x11-error-on-image-paste).

## Why does Claude say Remote Control is "disabled by your organization's policy"?

That's cc-screen, not your employer. Sessions cc-screen launches run Claude
Code with its own remote control (claude.ai/code and the Claude mobile app)
switched off, so a session you started here is reachable through cc-screen
and nowhere else — one remote-access layer per session. Claude Code phrases
every block that way; we don't control the wording.

Want it on for a session? Create the session with the **Claude app** switch
on (the `claude` row in `ccs`) and it launches registered as
`claude-<name>`, badged in the session list. The choice sticks across
restarts, restores, and assistant updates. Existing sessions keep their
stance until you recreate them.

## What are the codes like `WDJB-MJHT`?

One-time machine-activation codes. The installer prints one; you approve it
at [app.ccscreen.dev/activate](https://app.ccscreen.dev/activate) while
signed in, which links the machine to your account. They expire after 10
minutes and the installer auto-requests a fresh one, so an expired code costs
nothing.

## Why did `n`/`x` (and `q`) stop working in the `ccs` switcher?

The switcher became **search-first**: typing now filters the session list, so
single letters type into the search instead of running commands. The actions
moved to chords — `Ctrl-N` new, `Ctrl-X` kill, `Ctrl-E` graceful exit,
`Ctrl-R` rename, `Ctrl-O` restore — and `Esc` clears the search, then quits.
The same applies inside the grid's action menu (`Ctrl-A d`): `j`/`k` now type
into the search there too, and the arrow keys still navigate.
The header line in the switcher always shows the current keys; see
[the ccs terminal client](../tui/) for the full map.

## What is the "Recent" list at the top of the session list?

The sessions you were last *working in*, most recent first — the answer to
"where was I two sessions ago?". It's deliberately **not** sorted by which
agent needs you: the whole point is that the top rows stay put, so switching
between the two or three sessions you're actively driving is muscle memory
rather than reading. Attention still surfaces everywhere it did before: the
ready toasts, the status view, and the freshest-first order inside each
machine group.

A few things people ask about it:

- **The session you're in isn't in it.** It's already on screen; the list is
  what you'd switch *to*. With a single pane that makes the first row the
  session you were in immediately before — open the switcher, press `Enter`,
  and you're back.
- **It's empty for a new user**, and stays empty until you've been in a
  session you're not currently in. Nothing to fix.
- **It doesn't sync between devices.** Your phone, your laptop's browser, and
  `ccs` each keep their own — "where was I" is a per-device question. The web
  app stores it in that browser (clearing site data clears it); `ccs` stores
  it in `~/.config/cc-screen-tui/state.toml`, keyed per hub (delete the file
  to clear it).
- **`ccs` and the web app on the same machine show different lists** for the
  same reason: two client installs, two histories.
- **A session on an offline machine doesn't fall out of it.** It's skipped
  while it's missing and comes back where it was. Only killing or exiting a
  session removes it, and the list holds 20 (showing 10).

## Why does scrolling behave differently when an app like Claude or vim is running?

Because the app has taken over the screen. Full-screen apps — Claude Code's
fullscreen renderer, `vim`, `less`, `htop` — draw on the terminal's alternate
screen, which keeps no scrollback of its own, so in `ccs` the wheel is handed
to the app and *its* view scrolls instead of the pane's history. Back at a
shell prompt the wheel scrolls the pane's own multi-thousand-line scrollback
as usual. The rules are in [the ccs terminal client](../tui/#scrolling); if
the wheel does nothing at all, see
[Troubleshooting](../troubleshooting/#scrolling-does-nothing-in-a-claude-session).

The web app follows exactly the same three rules, for touch and for the mouse
wheel: your own scrollback at a shell prompt, the application's view once it has
taken the screen.

## Why did my copy disappear?

If you copied *inside* a session — Claude's `/copy`, or its copy-on-select — the
answer used to be "it went nowhere, or to the wrong machine". A terminal copy
travels as an escape sequence for the terminal to act on, and older cc-screen
clients discarded it; on a Mac agent, Claude Code's own fallback wrote the
*agent machine's* pasteboard instead of yours. Both are fixed: the copy now
lands on the device you're holding, in the web app and in `ccs`. The details,
including the cases that still need one click, are in
[Troubleshooting](../troubleshooting/#i-copied-inside-claude-and-my-clipboard-is-empty).

The reverse — a session *reading* your clipboard — is not something cc-screen
does. Terminals can be asked for their clipboard contents; cc-screen never
answers, on any client.

## Is cc-screen open source? What's it built with?

The code lives at
[github.com/dibbla-agents/cc-screen-rust](https://github.com/dibbla-agents/cc-screen-rust).
The agent, hub, and terminal client are Rust; the web app is React, embedded
into the binaries so there's nothing separate to deploy. Billing ships open in
the same MIT repo — it's simply off without Stripe keys, so a self-hosted hub
behaves exactly as the hosted one did before payments existed.
