# The hub — one address for all your machines

By default cc-screen-rust is **one machine**: you run an *agent* on the box where
your AI coding CLIs live, and you open that box directly (its tailnet IP, port
8839) from a phone, browser, or the `ccs` TUI.

The **hub** lets you put **one address in front of many machines**. Each machine
runs its agent as before, but the agent also *dials out* to a central hub and
registers itself. You point your browser or `ccs` at the **hub**, and you see
**every machine's sessions in one list**, each tagged with the machine it lives
on — attach to any of them, manage their lifecycle, browse/edit their files, and
get one push subscription for the whole fleet.

```
   phone / browser / ccs                  the hub                     your machines
   ─────────────────────                  ───────                     ─────────────
        │                                   │                       ┌─ agent (laptop) ── claude, codex …
        │   one URL (https://hub:8840)      │  ◀── dials out ───────┤
        └──────────────────────────────────┤  ◀── dials out ───────┼─ agent (server) ── gemini …
                                            │  ◀── dials out ───────┤
                                            │                       └─ agent (pi) ────── kimi …
        relays each request ───────────────┘
        to the owning machine
```

Three pieces:

- **agent** (`cc-screen-rust`) — runs on each machine; owns that machine's PTYs
  and files. It can run **stand-alone** (today's behavior) *and/or* dial a hub.
- **hub** (`cc-screen-hub`) — the aggregator. Owns **no** PTYs and **no** files;
  it's a registry + auth gate + transparent relay. Default port **8840**.
- **clients** — the **React PWA** (served by the hub *and* by each agent) and the
  **`ccs`** terminal client. Both speak to the hub exactly as they'd speak to a
  single agent.

The agent is sometimes called a "slave" in the sense that it reports up to the
hub — but it's a full agent: it keeps owning its sessions, and (unless you pass
`--hub-only`) it still serves clients directly too.

---

## Quick start

**1 — Run the hub** on whichever box should be the front door (often the same box
as one of the agents — the hub's 8840 coexists with an agent's 8839):

```sh
# install ccs / agent first (see README); then on the hub box:
cc-screen-hub install --password 'choose-a-passphrase' \
    --agents 'laptop:LAPTOP_TOKEN,server:SERVER_TOKEN'
# → serves on the tailnet IP:8840, client login = the password, and only
#   'laptop'/'server' (with those tokens) may register.
```

**2 — Point each machine's agent at the hub:**

```sh
# on the laptop:
cc-screen-rust install --hub https://hub.example:8840 \
    --hub-token LAPTOP_TOKEN --machine-id laptop
# on the server:
cc-screen-rust install --hub https://hub.example:8840 \
    --hub-token SERVER_TOKEN --machine-id server
```

That's it — open `https://hub.example:8840` in a browser (log in with the
password, Add to Home Screen on a phone), or:

```sh
ccs --server https://hub.example:8840 --token <client-token>
```

You'll see `laptop` and `server` sessions in one switcher.

---

## The hub in detail

### Install

```sh
cc-screen-hub install [--port N] [--bind ADDR] [--password PW] [--token TOK] [--agents SPEC]
cc-screen-hub uninstall
cc-screen-hub --help              # runtime usage
cc-screen-hub install --help      # install flags
```

`install` writes `~/.config/cc-screen-hub/web.env`, installs a service (systemd
`--user` on Linux, launchd on macOS), and (re)starts it. Re-running `install`
preserves keys you don't override.

| flag | meaning |
|------|---------|
| `--port N` | port (default **8840**, so it coexists with an agent on 8839) |
| `--bind ADDR` | bind address (default: the tailnet IP, else `127.0.0.1`) |
| `--password PW` | turn on the **client** auth gate (browser/TUI login; 2-week cookie) |
| `--token TOK` | the **client** API token (for `ccs`/scripts); auto-minted if you set `--password` without one |
| `--agents SPEC` | the **per-agent uplink tokens**, `machine:token,machine2:token2` |

### Uplink: open vs gated

`CCHUB_AGENT_TOKENS` (set by `--agents`) controls who may register:

- **empty (open uplink)** — *any* agent that connects may register. Fine on a
  trusted tailnet / for local dev. **Not** for off-tailnet.
- **set** — an agent must present its machine's exact token (and only listed
  machines are accepted). Use this whenever the hub is reachable beyond a tailnet.

### Two independent credentials

The hub has **two** separate secrets — don't mix them up:

- the **client gate** (`CCWEB_PASSWORD` / `CCWEB_API_TOKEN`) — what a *browser or
  `ccs`* uses to talk to the hub;
- the **per-agent uplink tokens** (`CCHUB_AGENT_TOKENS`) — what each *agent* uses
  to register.

A leaked client password can't impersonate an agent; a leaked agent token scopes
to one machine.

---

## The agent ("slave") in detail

Same binary as the stand-alone server; the hub flags are additive.

```sh
# stand-alone (unchanged):
cc-screen-rust install

# also report into a hub:
cc-screen-rust install --hub https://hub:8840 --hub-token TOK --machine-id NAME
```

| flag | env | meaning |
|------|-----|---------|
| `--hub URL` | `CCWEB_HUB_URL` | hub to dial out to and register with |
| `--hub-token TOK` | `CCWEB_HUB_TOKEN` | this machine's uplink token (must match the hub's `--agents`) |
| `--machine-id NAME` | `CCWEB_MACHINE_ID` | name shown in the hub's list (default: hostname) |
| `--hub-only` | `CCWEB_HUB_ONLY` | bind **no** local port — reachable *only* through the hub |

**Dual-mode vs `--hub-only`.** Without `--hub-only`, the agent *also* keeps
serving directly on the tailnet (so you can still hit `http://laptop:8839`). With
`--hub-only`, the agent binds no inbound socket at all — it only dials out — which
is the strictest posture for a box running YOLO agents, since it stops listening
entirely.

The agent always **owns its PTYs locally**: a hub restart never kills your
sessions (the agent just reconnects), and the resume-on-restart behavior is
unchanged.

---

## Clients

- **Browser / PWA.** Open the hub URL (e.g. `https://hub:8840`). The hub serves
  the same PWA the agent does, plus the unified session list. Add to Home Screen
  for the phone experience; one push subscription covers all machines.
- **`ccs` (TUI).** `ccs --server https://hub:8840 --token <client-token>`. The
  switcher shows every machine's sessions (machine-tagged); the multi-pane grid
  can even show panes from *different machines* side by side. The server URL +
  token are saved to `~/.config/cc-screen-tui/config.toml`, so later `ccs` (no
  args) reconnects. See `ccs --help`.

---

## What's relayed

Through the hub, today:

- ✅ **terminal** — attach, snapshot, input, output, per-client resize
- ✅ **session lifecycle** — create / delete / key / paste / clear-history /
  restore / restorable / session-root, namespaced by machine
- ✅ **file browser + editor** — list dirs/files, read/write/delete, mkdir,
  rmdir, rename ($HOME-confined on the agent)
- ✅ **filesystem watch** — the editor's live file tree
- ✅ **push** — centralized: agents report "an agent finished its turn", the hub
  buzzes subscribed devices with the machine name in the title

**Not yet relayed** (use the agent directly for these, or wait for the follow-up):

- ⏳ **bulk binary transfers** — file *download* (incl. range / PDF viewer), large
  *upload* (up to 500 MiB), and clipboard-image paste. These need a dedicated
  bulk stream; small file read/write (≤5 MiB text) already works through the hub.
- ⏳ **PWA machine UI polish** — the `ccs` TUI is fully machine-aware; the React
  PWA's `wsURL` accepts a machine, but its components (session-list badges, the
  new-session machine picker) still need `machine` threaded for the full
  multi-machine browser experience.

---

## Security model

The hub challenges the "tailnet-only, never bind public" rule deliberately:

- **Agents only dial out.** With `--hub-only` an agent binds nothing — stricter
  than a stand-alone agent, which at least listens on the tailnet.
- **The hub concentrates access.** Whoever controls the hub can drive every
  connected agent's PTYs and (relayed) files — hub compromise = fleet blast
  radius. So: enable the client gate in multi-machine setups, use per-agent uplink
  tokens, bind the hub's tailnet IP by default, and for **off-tailnet** use front
  the hub with a **TLS reverse proxy** (and prefer mTLS on the uplink). The hub
  owns no filesystem and runs no agent code itself, which bounds its own surface.
- **Confinement stays on the agent.** File ops run on the owning agent and go
  through its `$HOME` confinement — the hub can't widen it.

---

## Environment / `web.env` reference

Agent (`~/.config/cc-screen-rust/web.env`):

| key | meaning |
|-----|---------|
| `CCWEB_ADDR` | bind address (default `127.0.0.1:8839`) |
| `CCWEB_PASSWORD` / `CCWEB_API_TOKEN` | opt-in client auth (the agent's own gate) |
| `CCWEB_HUB_URL` | hub to register with (slave mode) |
| `CCWEB_HUB_TOKEN` | this machine's uplink token |
| `CCWEB_MACHINE_ID` | name in the hub's list (default hostname) |
| `CCWEB_HUB_ONLY` | `1`/`true` → bind no local port |

Hub (`~/.config/cc-screen-hub/web.env`):

| key | meaning |
|-----|---------|
| `CCWEB_ADDR` | bind address (default `127.0.0.1:8840`) |
| `CCWEB_PASSWORD` / `CCWEB_API_TOKEN` | client auth gate |
| `CCHUB_AGENT_TOKENS` | per-agent uplink tokens, `machine:token,…` (empty = open) |

---

## Updating

Each binary re-runs its hosted installer and (for the services) restarts onto the
new build:

```sh
cc-screen-hub  update     # on the hub box
cc-screen-rust update     # on each machine
ccs            update     # the TUI
```

## Troubleshooting

- **A machine isn't in the list.** Check the agent's log for `uplink: ...`; a bad
  token logs "rejected (bad uplink token)" on the hub. Confirm the hub's
  `--agents` entry name matches the agent's `--machine-id`, and the token matches.
- **"machine offline" when attaching.** The agent's uplink dropped; the hub keeps
  the last session list (greyed) and the agent auto-reconnects with backoff. The
  session itself is unharmed (the agent still owns the PTY).
- **Browser can't log in.** The hub needs `CCWEB_PASSWORD`/`CCWEB_API_TOKEN` set
  for the gate; with neither set the hub is open (tailnet-only).
- **Off-tailnet over plain http drops the cookie.** Use a TLS reverse proxy that
  sets `X-Forwarded-Proto: https` (the cookie is `Secure` only then).

---

## Local two-process smoke (no install)

Run both binaries under a throwaway `$HOME` on loopback ports — never the live
8839/8840, and never `install`/`uninstall` (that hits the real service):

```sh
TMP=$(mktemp -d); export HOME=$TMP; mkdir -p "$TMP/work"
./target/release/cc-screen-hub  --addr 127.0.0.1:18840 &
./target/release/cc-screen-rust --addr 127.0.0.1:18839 \
    --hub http://127.0.0.1:18840 --machine-id smoke --no-restore &
# create a shell session, then:
curl -s 127.0.0.1:18840/api/sessions          # lists it, tagged machine=smoke
# attach / watch via the example clients:
cargo run --example hub_attach_smoke -- 'ws://127.0.0.1:18840/api/ws?machine=smoke&session=shell-…'
cargo run --example hub_watch_smoke  -- 'ws://127.0.0.1:18840/api/watch?machine=smoke' "$TMP/work"
```

---

## How it works (pointer)

The load-bearing idea: every client maps 1:1 to a real `register_client()`
subscriber on the owning agent, tunneled over a logical channel inside the
agent↔hub WebSocket, so the engine's invariants (atomic snapshot, per-client
min-size resize, `Lagged`→resync) hold across the relay and `engine.rs` is
untouched. The envelope is in `crates/protocol/src/hub.rs`; the agent side is
`src/uplink.rs` + `src/attach.rs`; the hub is `crates/hub/`. See **AGENTS.md →
"The hub (aggregator)"** for the design and the security amendment.
