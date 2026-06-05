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

The image sets `HOME=/home/app`, so persisted state lives at
`/home/app/.config/cc-screen-hub` — mount a volume there (the compose file does)
to keep the cookie-signing key, favorites, and Web Push keys across restarts.

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
