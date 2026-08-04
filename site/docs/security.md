---
title: Security model
nav: Security model
description: What the hub can and cannot see, what the agents run with, where credentials live, and the transport rules — cc-screen's security model, honestly stated.
---

# Security model

cc-screen's security model is short enough to state honestly. This page is
it — what the hub sees, what the agents do, where the credentials live, and
what that means for you.

## The hub owns no terminals and no files

The hub — hosted or self-run — is a **registry and a byte relay**. It holds
no PTYs, runs no agent code, and has no filesystem access of its own. Every
file operation executes on the *owning machine*, inside that machine's
home-directory confinement — the hub cannot widen it. What transits the hub
is the relayed view: terminal bytes, file contents you open, session
metadata.

The honest flip side: **whoever controls a hub can drive every machine
connected to it** — that's the product working as designed, and it is exactly
why you choose who runs your hub. Use the hosted one, or
[run your own](../self-hosting/); the software is the same.

## The agents run YOLO — treat the machine accordingly

By default, cc-screen launches the coding CLIs with their approval-bypass
flags (Claude Code's is literally named `--dangerously-skip-permissions`).
That's the point of unattended agents — they don't stop to ask — but it means
a session can do anything your user account can do on that machine.

- Each session has a **Skip permissions** switch at creation (default on).
  Turn it off for a supervised session; it launches without the bypass flag
  and wears a "safe" badge.
- For real containment, run the machine host **in a container**: the
  container plus its mounted home volume become the sandbox, and a runaway
  agent is confined there. See
  [Self-hosting → Docker](../self-hosting/#docker).
- File operations through cc-screen's browser/editor are confined to the
  machine user's home directory. (The CLIs themselves are processes on the
  box — the confinement bounds cc-screen's file API, not what you ask an
  agent to do.)

## Credentials, and where they live

- **Your account** authenticates to the hub with a password (or Google) and
  rides a signed session cookie. Headless clients (`ccs`, scripts) use a
  bearer token instead.
- **Each machine** has its own uplink credential, minted when you approve its
  enrollment code. It's stored on the machine at
  `~/.config/cc-screen-rust/enroll.json`, written with owner-only (`0600`)
  permissions (on Windows the filesystem's protections are weaker — the file
  relies on your user profile's ACLs).
- The two are independent by design: a leaked account password can't
  impersonate a machine, and a machine's credential is scoped to that one
  machine — **Rotate** or **Unlink** on the dashboard kills it instantly.
- Enrollment codes (`XXXX-XXXX`) are short-lived (10 minutes), single-use,
  and approving one requires a logged-in browser — possession of the code
  alone links a machine to *your* account only if *you* approve it.

## Transport: when TLS is required

The agent fully trusts whatever answers at its hub URL — once connected, it
executes what the hub sends. So:

- **Across the internet, the uplink must be `wss://`** (an `https://` hub URL
  derives one). Certificates are validated against the standard root store
  and a bad certificate **fails closed** — the agent won't connect. This is
  what stops anyone from impersonating your hub. The hosted hub is TLS-only.
- **On a trusted private network** (a VPN/tailnet), plain `ws://` is
  acceptable — the network is the authenticated transport.

Machines connected the standard way accept **no inbound connections at all**:
the agent runs hub-only, binding no port, only dialing out.

## Fail-closed by default (self-hosted)

The binaries refuse insecure-by-accident setups rather than warning about
them:

- An agent won't bind a routable address with auth disabled.
- A hub won't start with an open uplink (no per-agent tokens) — even on
  loopback, because loopback hubs get fronted by tunnels. Each has an
  explicit env-var override for genuine dev use.
- Both reject cross-origin and DNS-rebinding browser requests independent of
  the auth gate.

Details and the exact variables: [Self-hosting](../self-hosting/).

## What we'd tell a security reviewer

- Your code and your CLI logins live on your machines and never rest on the
  hub. The hub relays views of them while you look.
- Session traffic through the hosted hub is TLS in transit, relayed through
  memory.
- The blast radius of a compromised machine credential is that machine; of a
  compromised account, the machines it owns *plus anything shared to it* —
  scope your shares deliberately ("can view" vs "can use").
