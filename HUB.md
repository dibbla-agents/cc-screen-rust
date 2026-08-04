# The hub — one address for all your machines

The **hub** (`cc-screen-hub`) puts one address in front of many machines: each
machine's agent (`cc-screen-rust`) *dials out* to the hub and registers, and
clients — the web PWA and the `ccs` TUI — talk to the hub, which relays each
request to the owning machine. The hub owns **no** PTYs and **no** files; it's
a registry + client-auth gate + transparent byte relay.

```
   phone / browser / ccs ──► the hub ◀── dials out ── agent (laptop) ── claude, codex …
                                    ◀── dials out ── agent (server) ── gemini …
```

**The end-user/operator documentation lives on the docs site** — this file is
just the pointer:

- **Self-hosting guide** (quick start, install flags, uplink tokens, Docker,
  Cloudflare-tunnel recipe, the `web.env` reference, updating):
  <https://ccscreen.dev/docs/self-hosting/>
- **Security model** (what the hub can/can't see, YOLO agents, credentials at
  rest, TLS rules, fail-closed binds):
  <https://ccscreen.dev/docs/security/>

The docs sources are in this repo under `site/docs/` — fix facts there, not
here.

---

## Local two-process smoke (no install) — contributors

Run both binaries under a throwaway `$HOME` on loopback ports — never the live
8839/8840, and never `install`/`uninstall` (that hits the real service):

```sh
TMP=$(mktemp -d); export HOME=$TMP; mkdir -p "$TMP/work"
# An open uplink (no per-agent tokens) is opt-in even on loopback, so set the
# override for this token-less dev run — otherwise the hub refuses to start.
CCHUB_ALLOW_OPEN_UPLINK=1 ./target/release/cc-screen-hub  --addr 127.0.0.1:18840 &
./target/release/cc-screen-rust --addr 127.0.0.1:18839 \
    --hub http://127.0.0.1:18840 --machine-id smoke --no-restore &
# create a shell session, then:
curl -s 127.0.0.1:18840/api/sessions          # lists it, tagged machine=smoke
# attach / watch via the example clients:
cargo run --example hub_attach_smoke -- 'ws://127.0.0.1:18840/api/ws?machine=smoke&session=shell-…'
cargo run --example hub_watch_smoke  -- 'ws://127.0.0.1:18840/api/watch?machine=smoke' "$TMP/work"
```

## How it works (pointer) — contributors

The load-bearing idea: every client maps 1:1 to a real `register_client()`
subscriber on the owning agent, tunneled over a logical channel inside the
agent↔hub WebSocket, so the engine's invariants (atomic snapshot, per-client
min-size resize, `Lagged`→resync) hold across the relay and `engine.rs` is
untouched. The envelope is in `crates/protocol/src/hub.rs`; the agent side is
`src/uplink.rs` + `src/attach.rs`; the hub is `crates/hub/`. See **AGENTS.md →
"The hub (aggregator)"** for the design and the security amendment.
