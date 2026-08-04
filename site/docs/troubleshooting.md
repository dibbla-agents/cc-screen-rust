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

You hit your plan's cap (free: 10 machines, 50 concurrent sessions). The app
shows which cap you hit and your current plan. Unlink a machine (or stop a
session) you no longer need, or request an upgrade — plans are free during
the beta and upgrades are handled manually, so it's a short email, not a
checkout.

## Assistants are outdated

Use the **Update** button on the machine's dashboard row — it updates the
CLIs first (a failed update leaves the machine untouched), then gracefully
restarts the sessions that use them, and your panes re-attach under the same
names. On the box: `cc-screen-rust doctor --update` (updates the CLIs only,
restarts nothing).

## Notifications don't arrive

- On iOS, Web Push only works for a PWA **installed to the home screen** —
  the notifications button hides until then.
- Re-check the permission: the bell button shows whether the subscription is
  active, and has a "send a test" action.

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
