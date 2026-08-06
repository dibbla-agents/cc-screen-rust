---
title: FAQ
nav: FAQ
description: Short answers — pricing, plans, supported CLIs, phones, privacy, and what happens when things restart.
---

# FAQ

## What does it cost?

The **Free** plan is free forever — 2 machines, 5 concurrent sessions, no card
required. **Pro** is $10/month (or $96/year — $8/month) and raises that
to 10 machines and 50 concurrent sessions, with unlimited sharing. Payments are
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
dashboard's Sharing card. See
[Using the web app → Sharing](../web-app/#sharing).

## Does it work on phones?

That's half the point. The web app is a PWA — **Add to Home Screen** gives
you an app icon, full-screen terminals sized to the phone, the file
browser/editor, and push notifications when an agent finishes its turn (on
iOS, notifications require the home-screen install).

## What are the codes like `WDJB-MJHT`?

One-time machine-activation codes. The installer prints one; you approve it
at [app.ccscreen.dev/activate](https://app.ccscreen.dev/activate) while
signed in, which links the machine to your account. They expire after 10
minutes and the installer auto-requests a fresh one, so an expired code costs
nothing.

## Is cc-screen open source? What's it built with?

The code lives at
[github.com/dibbla-agents/cc-screen-rust](https://github.com/dibbla-agents/cc-screen-rust).
The agent, hub, and terminal client are Rust; the web app is React, embedded
into the binaries so there's nothing separate to deploy. Billing ships open in
the same MIT repo — it's simply off without Stripe keys, so a self-hosted hub
behaves exactly as the hosted one did before payments existed.
