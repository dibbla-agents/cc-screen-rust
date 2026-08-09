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
and **Kimi** (`kimi`) — plus a plain **shell** session, which is just your
shell on the box and always available. A machine only offers the CLIs it
actually has installed; the installer's `--assistants` flag (and the
dashboard's Install button) can install the missing ones. Self-hosters can
also declare custom tools per machine.

## Where does my code live? What does the hub see?

On your machines, full stop. The hub stores your account, your machine
registrations, and sharing grants — never your repositories or your CLI
logins. What passes *through* it is the relayed view: the terminal bytes and
file contents you're currently looking at. See
[Security model](../security/).

## What happens when the hub restarts?

Nothing, from your sessions' point of view. Your machines own their sessions;
the hub is a relay. Agents reconnect automatically and the session list comes
back. Same answer for your phone losing signal or your browser closing —
sessions run server-side on *your* box, not in the browser tab.

## And when the machine itself reboots?

The agent starts on boot (it's a background service) and resumes its recorded
sessions with each CLI's own resume flag — Claude Code sessions come back
with their conversation intact via `--continue`.

## Can I use cc-screen without the hosted service?

Yes — fully. Run a single agent and connect to it directly, or run your own
hub (even your own multi-tenant hub with signup and enrollment). Same
binaries, no feature gap in the core product. See
[Self-hosting](../self-hosting/).

## Can I share access with a teammate?

Yes — share a whole machine or a single session, as "can view" or "can use".
If the invitee has no account yet, you get an invite link to send them; the
share attaches when they sign up with that email. Revoke any time from the
dashboard's Sharing card. Or skip the one-off shares and start a **team** —
everyone on it sees everyone's sessions automatically, view-only. See
[Using the web app → Sharing](../web-app/#sharing).

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

## Why does scrolling behave differently when an app like Claude or vim is running?

Because the app has taken over the screen. Full-screen apps — Claude Code's
fullscreen renderer, `vim`, `less`, `htop` — draw on the terminal's alternate
screen, which keeps no scrollback of its own, so in `ccs` the wheel is handed
to the app and *its* view scrolls instead of the pane's history. Back at a
shell prompt the wheel scrolls the pane's own multi-thousand-line scrollback
as usual. The rules are in [the ccs terminal client](../tui/#scrolling); if
the wheel does nothing at all, see
[Troubleshooting](../troubleshooting/#scrolling-does-nothing-in-a-claude-session).

## Is cc-screen open source? What's it built with?

The code lives at
[github.com/dibbla-agents/cc-screen-rust](https://github.com/dibbla-agents/cc-screen-rust).
The agent, hub, and terminal client are Rust; the web app is React, embedded
into the binaries so there's nothing separate to deploy. Billing ships open in
the same MIT repo — it's simply off without Stripe keys, so a self-hosted hub
behaves exactly as the hosted one did before payments existed.
