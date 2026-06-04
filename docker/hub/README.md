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

## Publishing to GitHub Container Registry (GHCR)

You can publish under your **personal** account (`ghcr.io/erikknave/cc-screen-hub`)
or the **org** (`ghcr.io/dibbla-agents/cc-screen-hub`). Personal is simplest: you
own the namespace outright, which sidesteps the org's read-only Actions token.

Either way you need a **Classic PAT** with `write:packages` (and `read:packages`
to pull). The `gh` CLI's default token does **not** carry `write:packages`, so
mint a fresh PAT at <https://github.com/settings/tokens> — it is not `gh auth token`.

### Option A — push manually from your machine (works today)

```sh
echo "$GHCR_PAT" | docker login ghcr.io -u erikknave --password-stdin
docker build -t ghcr.io/erikknave/cc-screen-hub:0.3.4 \
             -t ghcr.io/erikknave/cc-screen-hub:latest \
             -f docker/hub/Dockerfile .
docker push ghcr.io/erikknave/cc-screen-hub:0.3.4
docker push ghcr.io/erikknave/cc-screen-hub:latest
```

(Swap `erikknave` → `dibbla-agents` to publish under the org instead; the PAT must
be able to write that org's packages.)

New GHCR packages default to **private** — to let others pull, make the package
public in your account's *Packages* settings, or have pullers `docker login` with
a `read:packages` PAT. The `org.opencontainers.image.source` label (added by the
workflow's metadata step) links the package back to this repo.

### Option B — GitHub Actions on tag

`.github/workflows/hub-image.yml` builds and pushes on every `v*` tag (and on
manual dispatch), tagging the image with the semver version and `latest`. It pushes
to `github.repository_owner` by default; set a repo variable **`GHCR_OWNER`**
(e.g. `erikknave`) to target a personal namespace.

**Token caveat:** the org's default Actions `GITHUB_TOKEN` is read-only (the same
reason the release workflow can't publish GitHub Releases itself), and it can't
write a *personal* namespace from an org repo anyway. So add a **Classic PAT** with
`write:packages` as a repo secret named `GHCR_PAT` — the login step prefers it.

### Pull + run a published image

```sh
docker run -d --name cc-screen-hub -p 8840:8840 --env-file .env \
  -v cc-screen-hub-config:/home/app/.config/cc-screen-hub \
  ghcr.io/erikknave/cc-screen-hub:latest
```
