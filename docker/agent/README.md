# Running an agent (machine host) in Docker

The **agent** is the `cc-screen-rust` server that owns the PTYs and launches the
coding CLIs (`claude`, `codex`, `gemini`, …) with `--dangerously-skip-permissions`
/ YOLO. Containerizing it is the recommended way to run it: the container + the
home volume you mount become the sandbox, so a runaway agent is confined there
instead of roaming your host.

> **Why Ubuntu, not Alpine:** the coding CLIs are Node packages and the projects
> they build expect a normal glibc userland (build-essential, git, python). Alpine
> (musl) breaks prebuilt npm binaries and a lot of dev tooling. The image builds
> the Rust binary on Debian and runs it on `ubuntu:24.04`.

> **Still tailnet-only:** this box runs YOLO agents. Never publish `:8839` on a
> public interface. The recommended setup below gives it **no inbound at all**.

## Recommended: hub-only (dial out to your hub)

The agent dials *out* to your hub and registers; clients reach it only through the
hub. The container exposes no port — it accepts no inbound, matching the security
model exactly.

```sh
cd docker/agent
cp .env.example .env
#   set CCWEB_MACHINE_ID, CCWEB_HUB_URL, CCWEB_HUB_TOKEN, CCWEB_HUB_ONLY=1,
#   and your assistant creds
mkdir -p home && sudo chown -R 10001:10001 home   # container user (uid 10001) owns it
docker compose up -d --build
```

The token must match this machine's entry in the hub's `CCHUB_AGENT_TOKENS`. If
the hub also runs in Docker on the same host, put both on one compose network and
use `CCWEB_HUB_URL=http://hub:8840`; otherwise point at the hub's reachable
(tailnet) address.

## Alternative: standalone (direct access)

Uncomment the `ports:` block in `docker-compose.yml` (it binds host **loopback**
only) and set `CCWEB_API_TOKEN` in `.env`. Reach the UI at `http://127.0.0.1:8839`
or point `ccs` at it. Leave `CCWEB_HUB_*` blank.

## The home volume (projects + creds + state)

Everything lives under the container user's home, mounted from `./home`:

| Path in container | What |
|---|---|
| `/home/app/<your-project>` | working dirs you create sessions in (file ops are confined to `$HOME`) |
| `/home/app/.claude`, `.codex`, `.gemini` | the CLIs' login/creds — persist an interactive `docker compose exec agent claude` login here |
| `/home/app/.config/cc-screen-rust/` | agent state: `sessions.json` (restore list), `session.key`, push keys |

Authenticate the assistants either by API key in `.env` **or** by logging in once
inside the container (creds then persist in the volume):

```sh
docker compose exec agent claude        # or: codex, gemini — follow the login prompt
```

## Customizing which tools appear

The built-in tools are `claude`, `kimi`, `gemini`, `codex`, `shell`. To change the
list (e.g. drop `kimi`, or add flags), drop a `tools.conf` at
`/home/app/.config/cc-screen-rust/tools.conf` (i.e. `./home/.config/cc-screen-rust/tools.conf`).
Format, one tool per line:

```
# cc_tool <cmd> <prefix> <launch-template>   ({name} = session short name)
cc_tool cc  claude "claude --dangerously-skip-permissions"
cc_tool gc  gemini "gemini -y"
cc_tool tt  shell  "${SHELL:-/bin/bash} -l"
# cc_tool_resume     cc "--continue"
# cc_tool_extra_dirs cc "--add-dir"
```

`kimi` isn't installed by default (its distribution varies) — add it to the
Dockerfile's `npm install -g` line (or however it ships) if you use it, or omit it
from `tools.conf`.

## Publishing to GHCR

Same as the hub — image name `ghcr.io/erikknave/cc-screen-agent`, needs a Classic
PAT with `write:packages`:

```sh
echo "$GHCR_PAT" | docker login ghcr.io -u erikknave --password-stdin
docker build -t ghcr.io/erikknave/cc-screen-agent:0.3.4 \
             -t ghcr.io/erikknave/cc-screen-agent:latest \
             -f docker/agent/Dockerfile .
docker push ghcr.io/erikknave/cc-screen-agent:0.3.4
docker push ghcr.io/erikknave/cc-screen-agent:latest
```

Heads up: this image is large (Ubuntu + Node + the CLIs + your toolchains) and the
first build is slow. Subsequent builds reuse the cargo + layer caches.
