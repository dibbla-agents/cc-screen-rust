---
name: environments
description: Run + manage the prod and test cc-screen-hub environments on this host — test is the host-native compiled hub on :8840 (the systemd unit, for fast iteration), prod is the dockerized hub on host :8841 (stable). Use to start/stop/restart either hub, try a feature on test before prod, or promote a tested change to prod. Driven by scripts/hubctl.sh.
when_to_use: The user wants to test a hub feature before it goes live, start/stop/restart the test (:8840) or prod (:8841) hub, check what's running, or promote (ship) a change from test to prod. Also "spin up prod", "rebuild prod", "is prod up?", "try this on test first", "restart the hub".
---

# cc-screen-hub: test vs prod environments

Two hub instances run side-by-side on this host so features get tried on **test**
before they touch **prod**. They are **fully isolated** — different ports,
different state — so test churn can never clobber prod's keys/favorites.

| Env | Port | What it is | State | How code lands |
|-----|------|-----------|-------|----------------|
| **test** | **8840** | the **host-native compiled** hub — the existing `cc-screen-hub.service` systemd unit, whose `ExecStart` points at this repo's `target/release/cc-screen-hub` | `~/.config/cc-screen-hub` (host dir) | `cargo build` + restart the unit — *is* HEAD |
| **prod** | **8841** | the **dockerized** hub (`docker/hub/docker-compose.yml`); container listens on 8840 internally, mapped to host **8841** | the `hub-config` docker volume (isolated by the container) | rebuild the image (`promote`) |

The bridge is one direction only: **promote** = rebuild the prod container from
the *current source* (the same source test just ran). No shared state, no auto-sync.

> Why test lives on 8840: the systemd unit was already running the repo's release
> binary there, so the fast loop is just "rebuild + bounce the unit." Isolation
> is free — prod runs in a container with its own volume, test in the host config
> dir. (A `CCWEB_CONFIG_DIR` override also exists in `crates/hub/src/config.rs` if
> you ever want two *host-native* hubs.)

## The one tool: `scripts/hubctl.sh`

```sh
scripts/hubctl.sh test  <build|start|stop|restart|status|logs>
scripts/hubctl.sh prod  <up|down|restart|promote|status|logs>
scripts/hubctl.sh both  status
```

It sources `~/.cargo/env`, runs from the repo root, and reads optional overrides
from `scripts/hubctl.env` (gitignored).

## The everyday loop: iterate on test, then promote

```sh
# 1. hack on the hub (crates/hub/…)
scripts/hubctl.sh test restart      # cargo build --release + systemctl --user restart → new code on :8840
#    → open http://<tailnet-ip>:8840 (or point ccs at it) and kick the tires
scripts/hubctl.sh test logs         # journalctl -f while you poke

# 2. happy with it? ship it to prod:
scripts/hubctl.sh prod promote      # docker compose up -d --build  →  new :8841 container
scripts/hubctl.sh both status       # confirm both
```

`test restart` = `cargo build --release -p cc-screen-hub` then `systemctl --user
restart cc-screen-hub.service` — the unit re-execs the freshly built binary.
(This is *not* `cc-screen-rust/-hub install` — never run `install`/`uninstall` to
test; those rewrite `web.env` and bounce the live service. A plain `systemctl`
restart after a manual build is the safe fast loop.)

## Starting / checking each env

```sh
scripts/hubctl.sh prod up           # start the prod container (first run seeds docker/hub/.env)
scripts/hubctl.sh prod status       # prod  :8841  UP    (docker: cc-screen-hub)
scripts/hubctl.sh test status       # test  :8840  UP    (systemd: cc-screen-hub.service, …)
scripts/hubctl.sh both status       # one-line view of both
```

`prod up` copies `docker/hub/.env.example` → `docker/hub/.env` on first run if
missing — **edit the tokens there before exposing the hub** (see HUB.md's security
model; the hub is fleet blast-radius).

## Configuration (`scripts/hubctl.env`, optional)

Defaults fit this host. Override by creating `scripts/hubctl.env` (sourced if present):

```sh
# scripts/hubctl.env
PROD_PORT=8841                       # host port the docker prod hub publishes (→ container 8840)
TEST_PORT=8840                       # cosmetic in status; the bind is in ~/.config/cc-screen-hub/web.env
TEST_UNIT=cc-screen-hub.service      # the systemd --user unit `test` drives
```

- **test** creds/bind live in `~/.config/cc-screen-hub/web.env` (`CCWEB_ADDR`,
  `CCWEB_PASSWORD`, `CCWEB_API_TOKEN`, `CCHUB_AGENT_TOKENS`) — edit there and
  `test restart`.
- **prod** creds live in `docker/hub/.env`; the host port is `HUB_HOST_PORT`
  (defaults to 8841, set by hubctl from `PROD_PORT`).

## Pointing agents / clients at an env

```sh
# a machine agent → test or prod, by port
cc-screen-rust --hub http://<host>:8840 --hub-token <tok> --machine-id <name>   # test
cc-screen-rust --hub http://<host>:8841 --hub-token <tok> --machine-id <name>   # prod
# ccs / browser
ccs --server http://<host>:8840   # test
ccs --server http://<host>:8841   # prod
```

## Gotchas

- **8840 is the systemd unit; 8841 is docker.** Don't point the docker prod at
  host 8840 — it'd collide with the test unit. The split is deliberate.
- **`promote` rebuilds from your working tree**, not a tag — commit first so prod
  matches a known commit. (Pinning prod to a release tag is the *other* promotion
  model; this skill assumes rebuild-the-image.)
- **Prod state is the `hub-config` volume** — don't `docker compose down -v` it
  unless you mean to wipe favorites / push subs / the cookie-signing key. Test
  state is `~/.config/cc-screen-hub`.
- **This is the hub only.** The PTY-owning `cc-screen-rust` agent (:8839) is
  deployed via `./install.sh` / the `release` skill — separate flow.
- Full hub setup, security model, and what's relayed live in **`HUB.md`**.
```
