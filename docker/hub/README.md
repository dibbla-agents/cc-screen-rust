# Running the hub in Docker

The **hub** aggregates many machine agents behind one endpoint. It owns no PTY and
no filesystem — just a registry + client-auth gate + byte relay — so it runs well
in a container. Clients (the PWA + `ccs`) talk to the hub; each machine host (the
`cc-screen-rust` agent) dials *out* to the hub and registers.

> Security model (see `HUB.md`): the hub concentrates access to every connected
> agent's PTYs/filesystem, so a compromised hub = fleet blast radius. For anything
> beyond a private tailnet: set a client credential (`CCWEB_API_TOKEN`), require
> per-agent uplink tokens (`CCHUB_AGENT_TOKENS`), and front it with a TLS reverse
> proxy. Never expose it to the public internet unauthenticated.

## Quick start (build locally)

```sh
cd docker/hub
cp .env.example .env          # then edit the tokens (see below)
docker compose up -d --build
```

The hub is now on `http://localhost:8840`. Open it in a browser, or point `ccs` at
it: `ccs --server http://HOST:8840 --token <CCWEB_API_TOKEN>`.

Build the image directly (without compose):

```sh
# from the repo root — the hub is a workspace member, so the context is the root
docker build -t cc-screen-hub -f docker/hub/Dockerfile .
docker run -d --name cc-screen-hub -p 8840:8840 --env-file docker/hub/.env \
  -v cc-screen-hub-config:/home/app/.config/cc-screen-hub cc-screen-hub
```

The image is built with `--features multi-tenant` by default: the SaaS
capability ships in every hub artifact but stays **dormant** until
`CCHUB_DATABASE_URL` is set (see below) — without it the hub behaves exactly
like the classic single-tenant hub. The only visible deltas of the feature-on
build are that the gated routes exist and answer honestly (e.g.
`POST /api/signup` → `501 "not a multi-tenant hub"` instead of `404`) and a
slightly larger binary. If you want the minimal build anyway, use the escape
hatch: `docker build --build-arg FEATURES= …`.

## Pull the prebuilt image (run it on another machine)

You don't have to build — CI publishes the hub image to GHCR on every release tag.
**Canonical image:** `ghcr.io/dibbla-agents/cc-screen-hub` (tags: the semver
version, e.g. `0.3.5`, and `latest`). It's a **public** package, so any machine
can pull it with no login:

```sh
docker pull ghcr.io/dibbla-agents/cc-screen-hub:latest          # or :0.3.5 to pin

docker run -d --name cc-screen-hub -p 8840:8840 --env-file .env \
  -v cc-screen-hub-config:/home/app/.config/cc-screen-hub \
  ghcr.io/dibbla-agents/cc-screen-hub:latest
```

Or with compose: the shipped `docker-compose.yml` already has `image:` set to that
tag, so dropping its `build:` block (or just `docker compose pull && up -d`) runs
the registry image instead of building. The host port defaults to **8840**;
override with `HUB_HOST_PORT=8841 docker compose up -d` to coexist with another
hub already on 8840.

> If the package is ever flipped back to private, pulling needs a one-time
> `echo "$PAT" | docker login ghcr.io -u <you> --password-stdin` with a
> `read:packages` token first.

## Configuration (env vars)

All optional. With everything blank the hub runs with **no auth** and an **open
uplink** — only acceptable on a trusted private network.

| Var | What | Example |
|-----|------|---------|
| `CCWEB_API_TOKEN` | Client "API key" — headless clients send `Authorization: Bearer <token>`; the web login accepts it too. | `openssl rand -hex 32` |
| `CCWEB_PASSWORD` | Web-login password (mints a 2-week cookie). Optional; the token alone gates everything. | `hunter2` |
| `CCHUB_AGENT_TOKENS` | Per-agent uplink tokens, `machine:token,m2:tok2`. Empty = any agent may register. **Separate** secret from the client gate. | `pine:abc,oak:def` |
| `CCWEB_ADDR` | Bind address inside the container. The image defaults it to `0.0.0.0:8840`; don't usually override. | `0.0.0.0:8840` |
| `CCWEB_ALLOWED_ORIGINS` | Extra allowed Origin/Host values (comma-separated) for a proxy/tunnel-fronted hub. List every public hostname clients use. | `app.example.com` |
| `CCHUB_DATABASE_URL` | **Turns on multi-tenant (SaaS) mode.** SQLite URL; keep the file in the config volume (convention below). Unset = single-tenant. | `sqlite:///home/app/.config/cc-screen-hub/hub.db` |
| `CCHUB_PUBLIC_URL` | Canonical public origin (no trailing `/`) baked into the served installers, the device flow's `verification_uri`, and OAuth redirects. | `https://app.example.com` |
| `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET` | Enable "Sign in with Google" (redirect URI `<CCHUB_PUBLIC_URL>/api/auth/google/callback`). | — |
| `CCHUB_OAUTH_ONLY` | Set to `1` to disable password signup/login (Google only). | `1` |
| `CCHUB_SUPPORT_EMAIL` | Optional support contact shown at plan-limit walls. | `support@example.com` |
| `CCHUB_SMTP_URL` | Relay that mails team/share invitations. Unset = the hub sends no mail and everything else works (the copyable invite link is the channel). Needs `CCHUB_PUBLIC_URL` too. | `smtp://user:key@smtp-relay.brevo.com:587` |
| `CCHUB_MAIL_FROM` | From/envelope address on invite mail; the relay must be authorized for it. Default `cc-screen <invites@ccscreen.dev>`. | `cc-screen <invites@example.com>` |
| `CCHUB_MAIL_REPLY_TO` | Reply-To on invite mail. Falls back to `CCHUB_SUPPORT_EMAIL`. | `support@example.com` |
| `CCHUB_MAIL_DIR` | Write each message to a file here instead of sending it (tests/capture). Takes precedence over `CCHUB_SMTP_URL`. | `/tmp/cc-screen-mail` |
| `CCHUB_SUMMARY_BUDGET` / `CCHUB_SUMMARY_USER_BUDGET` | Session-summary spend caps in USD (fleet-wide / per-user). | `50` / `2` |

The image sets `HOME=/home/app`, so persisted state lives at
`/home/app/.config/cc-screen-hub` — mount a volume there (the compose file does)
to keep the cookie-signing key, favorites, and Web Push keys across restarts.

## Multi-tenant (SaaS) mode

The shipped image can run as a multi-account service: public signup + Google
sign-in, per-user machine enrollment via `<hub>/activate`, per-plan
machine/session caps. It's runtime opt-in — set `CCHUB_DATABASE_URL` and
restart; unset it and the same image is the classic single-tenant hub.

**DB-in-volume convention:** point the SQLite file into the state dir the
compose file already persists, so the database rides the existing
`hub-config` volume with no extra mount:

```sh
CCHUB_DATABASE_URL=sqlite:///home/app/.config/cc-screen-hub/hub.db
```

Plans are manual (no billing): `cc-screen-hub user plan <email> free|pro|unlimited`
— run it inside the container with the same env, e.g.
`docker exec -e CCHUB_DATABASE_URL=sqlite:///home/app/.config/cc-screen-hub/hub.db cc-screen-hub cc-screen-hub user plan you@example.com pro`.

**Production deploy runbook (reproducible):** on the origin host, production is
the shipped image + this compose file + a `.env` — no hand-built binaries:

```sh
cd docker/hub
# .env holds the multi-tenant block: CCHUB_DATABASE_URL, CCHUB_PUBLIC_URL,
# GOOGLE_OAUTH_CLIENT_ID/SECRET, CCWEB_ALLOWED_ORIGINS (every public hostname), …
docker compose pull && docker compose up -d
```

`scripts/hubctl.sh` is the dev-box analogue of the same test/prod split.

## Connecting a machine host (agent) to this hub

On each machine running the `cc-screen-rust` agent:

```sh
cc-screen-rust --hub http://HUB-HOST:8840 --token <uplink-token> --machine-id pine
# (env equivalents: CCWEB_HUB_URL / CCWEB_HUB_TOKEN / CCWEB_MACHINE_ID)
```

`<uplink-token>` must match the one you listed for that machine in
`CCHUB_AGENT_TOKENS` (or any value if you left it open). Add `--hub-only` on the
agent to drop its own local bind so it's reachable *only* through the hub.

## Publishing to GHCR (how the image gets there)

**CI does this automatically.** `.github/workflows/hub-image.yml` builds and pushes
to **`ghcr.io/dibbla-agents/cc-screen-hub`** on every `v*` tag (and on manual
dispatch), tagging the semver version + `latest`. The owner is
`github.repository_owner` (the org); set a repo/org variable **`GHCR_OWNER`** to
target a personal namespace instead. So a normal release needs **no manual push** —
just tag a version (see the `release` flow) and the image follows.

**Token caveat:** the org's default Actions `GITHUB_TOKEN` is read-only (same
reason the release workflow can't publish GitHub Releases itself). If the push
step 403s, add a **Classic PAT** with `write:packages` as a repo/org secret named
`GHCR_PAT` — the login step prefers it.

**Visibility:** the package is **public** so anyone can `docker pull` it (the image
carries no secrets — creds come from `.env`/the volume at runtime). New GHCR
packages default to *private*; flip it once in the package's *Settings → Danger
Zone → Change visibility*. The `org.opencontainers.image.source` label links the
package back to this repo.

### Manual fallback (push by hand)

Only needed if CI is unavailable. Requires a Classic PAT with `write:packages`
(the `gh` CLI's default token does **not** carry it — mint one at
<https://github.com/settings/tokens>):

```sh
echo "$GHCR_PAT" | docker login ghcr.io -u <you> --password-stdin
docker build -t ghcr.io/dibbla-agents/cc-screen-hub:0.3.5 \
             -t ghcr.io/dibbla-agents/cc-screen-hub:latest \
             -f docker/hub/Dockerfile .
docker push ghcr.io/dibbla-agents/cc-screen-hub:0.3.5
docker push ghcr.io/dibbla-agents/cc-screen-hub:latest
```
