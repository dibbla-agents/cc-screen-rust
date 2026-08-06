---
title: Self-hosting
nav: Self-hosting
description: Run your own cc-screen hub and agents — quick start, uplink tokens, Docker, off-tailnet TLS, and the full environment reference.
---

# Self-hosting

Everything the hosted hub at `app.ccscreen.dev` does, you can run yourself.
Same binaries, same clients. Three shapes, smallest first:

1. **One machine, no hub.** Run the agent stand-alone and open it directly
   (its private/tailnet IP, port 8839) from a browser or `ccs`.
2. **A hub in front of many machines.** Each machine's agent *dials out* to
   your hub and registers; clients talk to the hub and see every machine's
   sessions in one list. This is the classic setup and most of this page.
3. **A multi-tenant hub** — your own miniature `app.ccscreen.dev`, with
   signup, Google sign-in, and per-user machine enrollment. One environment
   variable turns it on (below).

Read the [Security model](../security/) alongside this page — a hub
concentrates access to every connected machine, so its auth settings matter.

![Shape 2 — a hub in front of many machines, each dialing out to it](../img/docs-topology.svg "Shape 2: your hub is the one address clients talk to; every machine's agent dials out to it, so no machine needs an inbound port.")

## Quick start (hub + machines)

**1 — Install the hub** on whichever box should be the front door:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-hub-installer.sh | sh
cc-screen-hub install --password 'choose-a-passphrase' \
    --agents 'laptop:LAPTOP_TOKEN,server:SERVER_TOKEN'
```

`install` writes `~/.config/cc-screen-hub/web.env`, registers a service
(systemd `--user` on Linux, launchd on macOS), and starts it on port **8840**,
bound to the tailnet IP if there is one. Re-running `install` preserves keys
you don't override.

| flag | meaning |
|------|---------|
| `--port N` | port (default **8840**) |
| `--bind ADDR` | bind address (default: the tailnet IP, else `127.0.0.1`) |
| `--password PW` | turn on the **client** auth gate (browser/TUI login; 2-week cookie) |
| `--token TOK` | the **client** API token (for `ccs`/scripts); auto-minted if you set `--password` without one |
| `--agents SPEC` | the **per-agent uplink tokens**, `machine:token,machine2:token2` |

**2 — Point each machine's agent at it:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.sh | sh
cc-screen-rust install --hub https://hub.example:8840 \
    --hub-token LAPTOP_TOKEN --machine-id laptop
```

**3 — Open it.** Browse `https://hub.example:8840` (log in with the password;
Add to Home Screen on a phone), or `ccs --server https://hub.example:8840
--token <client-token>`. (If you run the hub *multi-tenant* — with a user
database — `ccs activate` device sign-in works against it too, exactly as on
the hosted service.)

## The agent's hub flags

Same binary as the stand-alone server; the hub flags are additive.

| flag | env | meaning |
|------|-----|---------|
| `--hub URL` | `CCWEB_HUB_URL` | hub to dial out to and register with |
| `--hub-token TOK` | `CCWEB_HUB_TOKEN` | this machine's uplink token (must match the hub's `--agents`) |
| `--machine-id NAME` | `CCWEB_MACHINE_ID` | name shown in the hub's list (default: hostname) |
| `--hub-only` | `CCWEB_HUB_ONLY` | bind **no** local port — reachable *only* through the hub |

Without `--hub-only` the agent also keeps serving clients directly on its own
port. With it, the agent binds no inbound socket at all — it only dials out —
the strictest posture for a box running unattended agents, and what the
hosted installer uses.

The agent always **owns its sessions locally**: a hub restart never kills
them; the agent just reconnects.

## Two independent credentials

Don't mix these up:

- the **client gate** (`CCWEB_PASSWORD` / `CCWEB_API_TOKEN`) — what a browser
  or `ccs` uses to talk to the hub;
- the **per-agent uplink tokens** (`CCHUB_AGENT_TOKENS`, set by `--agents`) —
  what each agent uses to register.

A leaked client password can't impersonate an agent; a leaked agent token
scopes to one machine. With `CCHUB_AGENT_TOKENS` empty the uplink is **open**
(any agent may register) — the hub refuses to start that way unless you
explicitly set `CCHUB_ALLOW_OPEN_UPLINK=1`, which is for trusted-network dev
use only.

## Multi-tenant mode (your own app.ccscreen.dev)

The shipped hub binary and Docker image include multi-tenant support,
dormant by default. Give the hub a database and it becomes a multi-account
service:

```sh
CCHUB_DATABASE_URL=sqlite:///home/you/.config/cc-screen-hub/hub.db
CCHUB_PUBLIC_URL=https://cc.your-domain.example   # the canonical origin
```

You get public signup (email/password, optionally Google OAuth), per-user
machine enrollment — the hub serves its own `/install.sh` and `/install.ps1`
with its URL baked in, and machines are approved at `<your-hub>/activate` —
plus sharing and per-plan caps. Plans are assigned by hand
(`cc-screen-hub user plan <email> free|pro|unlimited`). Same code we run, free
to self-host — billing ships in the MIT repo behind the `multi-tenant` feature
and is simply off without Stripe keys; paying for the hosted hub is paying to
not run it yourself.

## Docker

Both images are published to GHCR on every release:

```sh
docker pull ghcr.io/dibbla-agents/cc-screen-hub:latest     # the front door
docker pull ghcr.io/dibbla-agents/cc-screen-agent:latest   # a containerized machine host
```

- **Hub:** mount a volume at `/home/app/.config/cc-screen-hub` (cookie keys,
  push keys, the multi-tenant DB) and set the env vars from the reference
  below. Compose files ship in the repo.
- **Agent:** the recommended containerized machine host runs hub-only (no
  inbound port at all); the mounted home volume is the sandbox — projects,
  CLI logins, and state all live there. Log the assistants in once with
  `docker compose exec agent claude` (or set API keys in `.env`).

Full operator detail (compose files, GHCR/CI publishing):
[docker/hub/README.md](https://github.com/dibbla-agents/cc-screen-rust/blob/main/docker/hub/README.md)
and
[docker/agent/README.md](https://github.com/dibbla-agents/cc-screen-rust/blob/main/docker/agent/README.md).

## Off-tailnet access: TLS required

On a private network (a VPN or a tailnet), plain `http://`/`ws://` is fine —
the network is the transport security. Beyond it, the hub must be fronted by
TLS: the agent fully trusts whatever answers at `--hub`, and `wss://` (with a
valid certificate) is what authenticates the hub to the agent. The uplink
client validates certificates against the standard root store and fails
closed on a bad one.

A concrete no-inbound-port recipe with a **Cloudflare Tunnel**:

1. Bind the hub to loopback: `cc-screen-hub install --bind 127.0.0.1`.
2. Run `cloudflared` on the same host with the public hostname's **Service**
   set to `http://127.0.0.1:8840`.
3. Browse / `ccs` at `https://your-hostname`. Cloudflare terminates TLS and
   sets `X-Forwarded-Proto: https`, so the hub's `Secure` login cookie works.

Gotchas:

- **502 Bad Gateway** almost always means the tunnel's origin can't reach the
  hub — e.g. the hub is bound to its tailnet IP (the install default) while
  the tunnel origin points at `localhost`. Align the two.
- **A loopback bind is not private once a tunnel fronts it.** A public hub
  needs per-agent uplink tokens (`--agents`) — the client password does *not*
  gate agent registration — and ideally an access policy at the edge too.
- **Browser 403s after fronting with your own domain?** Add the domain to
  `CCWEB_ALLOWED_ORIGINS` (the hub's cross-origin/DNS-rebinding guard only
  auto-accepts same-origin, raw IPs, `localhost`, and `*.ts.net`).

## Environment reference

Agent (`~/.config/cc-screen-rust/web.env`):

| key | meaning |
|-----|---------|
| `CCWEB_ADDR` | bind address (default `127.0.0.1:8839`) |
| `CCWEB_PASSWORD` / `CCWEB_API_TOKEN` | opt-in client auth (the agent's own gate) |
| `CCWEB_HUB_URL` | hub to register with |
| `CCWEB_HUB_TOKEN` | this machine's uplink token |
| `CCWEB_MACHINE_ID` | name in the hub's list (default hostname) |
| `CCWEB_HUB_ONLY` | `1`/`true` → bind no local port |
| `CCWEB_ALLOWED_ORIGINS` | extra allowed browser Origin/Host values (reverse-proxy domain), comma-separated |
| `CCWEB_ALLOW_UNAUTHENTICATED_REMOTE` | `1` → allow a routable bind with auth off (overrides the fail-closed guard) |
| `CCWEB_CSP` | override the embedded app's Content-Security-Policy (`off`/empty disables it) |

Hub (`~/.config/cc-screen-hub/web.env`):

| key | meaning |
|-----|---------|
| `CCWEB_ADDR` | bind address (default `127.0.0.1:8840`) |
| `CCWEB_PASSWORD` / `CCWEB_API_TOKEN` | client auth gate |
| `CCHUB_AGENT_TOKENS` | per-agent uplink tokens, `machine:token,…` (empty = open) |
| `CCHUB_ALLOW_OPEN_UPLINK` | `1` → allow an empty `CCHUB_AGENT_TOKENS`. Required for any no-token run, including loopback. Trusted networks/dev only. |
| `CCHUB_DATABASE_URL` | set → multi-tenant mode (accounts, enrollment, sharing) |
| `CCHUB_PUBLIC_URL` | the hub's canonical public origin (used by `/activate` instructions, OAuth, the served installers) |
| `CCWEB_ALLOWED_ORIGINS` | extra allowed browser Origin/Host values, comma-separated |
| `CCWEB_ALLOW_UNAUTHENTICATED_REMOTE` | `1` → allow a routable bind with client auth off |
| `CCWEB_CSP` | override the embedded app's Content-Security-Policy |

## Updating

Each binary re-runs its hosted installer and (for the services) restarts onto
the new build:

```sh
cc-screen-hub  update     # on the hub box
cc-screen-rust update     # on each machine
ccs            update     # the TUI
```

## Troubleshooting a self-hosted setup

- **A machine isn't in the list.** Check the agent's log for `uplink: …`; a
  bad token logs "rejected (bad uplink token)" on the hub. The hub's
  `--agents` entry name must match the agent's `--machine-id`, token and all.
- **Browser can't log in.** The hub needs `CCWEB_PASSWORD` /
  `CCWEB_API_TOKEN` set for the gate.
- **Off-tailnet over plain http drops the login cookie.** Use a TLS proxy
  that sets `X-Forwarded-Proto: https` — the cookie is `Secure`-only then.

More in [Troubleshooting](../troubleshooting/).
