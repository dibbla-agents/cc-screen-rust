---
title: Troubleshooting
nav: Troubleshooting
description: Symptom-shaped fixes — activation codes, offline machines, missing assistants, plan limits, and proxy 403s.
---

# Troubleshooting

Symptom first, fix second. Self-hosting problems (uplink tokens, TLS,
binds) have their own section on the [Self-hosting](../self-hosting/) page.

## "The activation code expired"

Codes live **10 minutes**. There's nothing to redo: the installer notices the
expiry and prints a fresh code by itself —

```
  Code expired before approval — requesting a new one…
```

— so just enter the new one at
[app.ccscreen.dev/activate](https://app.ccscreen.dev/activate). If you closed
the installer, re-run the one-liner; it's idempotent.

## "Approve machine" says login required

Approving binds the machine to *your* account, so the browser on `/activate`
must be signed in. Sign in first, then enter the code — on the same site,
any device.

## A session fails to start / an assistant isn't offered

The machine doesn't have that coding CLI installed. cc-screen tells you
rather than launching a doomed session: the picker greys the tool out, and
the machine's dashboard row shows **⚠ N missing · Install** — click it to
install the missing CLIs remotely (for that machine's user, no sudo). On the
box itself, the equivalent is:

```sh
cc-screen-rust doctor                  # what's installed, and where
cc-screen-rust doctor --install --yes  # install everything missing
```

On Windows, remember Codex and Gemini need Node.js first — see
[Install on Windows](../install-windows/).

## A machine shows offline

Its agent's connection to the hub dropped — the box is asleep, offline, or
rebooting. Nothing is lost: the agent owns its sessions locally, keeps them
running, and reconnects with backoff the moment it can. Sessions re-attach by
themselves. If it stays offline, check that the machine is actually up and
that the background service is running (`systemctl --user status
cc-screen-rust` on Linux).

## "Machine limit reached for your plan"

You hit your plan's cap. The app shows which cap you hit and your current plan,
and offers the upgrade right there — checkout to Pro happens in-app. Or unlink a
machine (or stop a session) you no longer need. **Nothing is deleted** when you
hit a limit: existing machines stay enrolled, running sessions keep running, and
your files are never touched — the cap only blocks *new* machines and sessions
until you're back under it.

## "This team is out of seats"

Accepting the invite would take the team past its paid seat count. The
team's **owner or an admin** fixes it — add seats from Billing (the Stripe
portal), or remove a member to free one. **Nothing was lost**: the invite
stays valid and the accept simply works once a seat is free.

## "Team machine pool full" / "Team session limit reached"

Team limits are **pooled**: 10 machines and 50 concurrent sessions per seat,
counted across the whole team — so you can hit the cap even if *you* have
only one machine, because a teammate is using the headroom. Same rule as
every limit: nothing is deleted; existing machines stay enrolled and running
sessions keep running. Free the pool (unlink a machine, stop a session,
anyone's) or have the owner add seats. Re-enrolling a machine the team
already has never counts against the pool.

## A teammate's machine isn't visible

Three usual causes, in order:

1. **They hid it.** Each machine has an owner-only "Visible to team" toggle —
   ask them to check the team window.
2. **Membership isn't active yet.** An invited-but-not-accepted member sees
   (and shows) nothing — the invite must be accepted, and the team needs a
   free seat for that.
3. **A freshly enrolled machine hasn't propagated.** Normally visibility is
   immediate; in the rare miss, a nightly reconcile heals it. If it's still
   missing the next day, that's a bug — please report it.

## Assistants are outdated

Use the **Update** button on the machine's dashboard row — it updates the
CLIs first (a failed update leaves the machine untouched), then gracefully
restarts the sessions that use them, and your panes re-attach under the same
names. On the box: `cc-screen-rust doctor --update` (updates the CLIs only,
restarts nothing).

## Codex says "clipboard unavailable" / an X11 error on image paste

Older cc-screen agents delivered every pasted image the same way, and Codex
tried to read it from the machine's X11/Wayland clipboard — which a headless
box doesn't have, so the paste failed with a clipboard/X11 connection error.
Current agents deliver images to Codex as a staged local file instead, so
**update the agent on that machine** (the machine dashboard row → Update, or
re-run the install one-liner) and the paste works with no display server.
You do **not** need to install Xvfb or any X11 packages.

## Notifications don't arrive

- On iOS, Web Push only works for a PWA **installed to the home screen** —
  the notifications button hides until then.
- Re-check the permission: the bell button shows whether the subscription is
  active, and has a "send a test" action.

![The notification bell in the switcher header](../img/mobile-notifications.png "The bell lives in the switcher header — it shows whether push is active and can send a test notification.")

## The page looks stale after an update

The web app is a PWA; after a deploy the service worker can serve the old
bundle for one more load. Close and reopen the app (or reload twice).

## Browser gets 403 behind my own proxy (self-host)

Your hub rejects unknown browser origins as an anti-DNS-rebinding measure.
Add your domain to `CCWEB_ALLOWED_ORIGINS` — see
[Self-hosting](../self-hosting/#off-tailnet-access-tls-required).

## Still stuck?

Check the [FAQ](../faq/), or open an issue at
[github.com/dibbla-agents/cc-screen-rust](https://github.com/dibbla-agents/cc-screen-rust).
