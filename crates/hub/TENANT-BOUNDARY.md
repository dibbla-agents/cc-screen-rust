# The tenant boundary — route × scope inventory and invariants

> **Status note (2026-08-06).** This document satisfies proposal 0042's
> **Stream A** acceptance ("an inventory table: every client-facing route ×
> {resolves an agent? via `resolve_scoped`? scope from cookie?}") and
> **Stream I**'s written bar (the invariant set sharing must preserve). It is
> maintained beside the code it describes: `crates/hub/src/lib.rs`
> (`build_router`) is the route source of truth — a route added there MUST get a
> row here.

The multi-tenant hub (`--features multi-tenant`) serves mutually-distrusting
tenants, every one of whom runs YOLO coding agents. A single un-scoped resolve
is therefore cross-account RCE, not an info leak. The boundary is concentrated
in one seam:

- `handlers::require_client_auth` derives the caller's identity from the
  **session cookie** or a **0060 client token** (Bearer, `client_tokens` only —
  never an agent uplink token), loads their full `Visibility` (ownership +
  0039/0065 grants) in one place, and stashes it as a request extension.
  Client-supplied input (query, headers, body) can never widen it.
- `registry::resolve_scoped(vis, machine, session)` is the only way a handler
  turns a machine label / session name into an agent connection. It filters to
  the caller's visibility and **refuses on ambiguity** — a second match returns
  `None`, never "pick one".

## Stream I invariants (the bar sharing and every future grant must clear)

1. **Default-deny.** A tenant's scope is exactly the agents carrying their
   `user_id` plus their explicitly-granted rows. No route resolves an agent
   outside that scope; the failure mode is refusal (`404`/`403`/no upgrade),
   never fall-through to another tenant's machine.
2. **Every grant is an explicit widening for a named resource.** A `shares` row
   names one agent (or one session on it) and one grantee. Nothing implicit: a
   pending invite grants nothing; only an accepted row enters `Visibility`.
3. **No path yields `Visibility::All` for a tenant.** `All` exists only in the
   single-tenant build (and `Registry::resolve` delegating with it). In
   multi-tenant, every gated request carries `Visibility::User`; exempt paths
   without a session get `Visibility::user("")`, which matches no agent.
4. **Revocable.** Visibility is re-derived from the store on every request, so
   revoke/leave/remove takes effect on the next request — no cached grant
   outlives its row.
5. **Audited.** Org-mediated actions append to the org audit log
   (`org.rs` action vocabulary); share create/revoke by org members likewise.
6. **Team grants are view-only materialized rows** (`kind='team'`, 0065):
   machine-wide *view* (list + attach), forward-inclusive for new sessions —
   but `may_use_agent` and `owns_agent` never match them: no create, no
   restore, no re-share, no administration.
7. **Control requires an explicit use-share.** Only ownership or a
   `kind='agent'` grant satisfies `may_use_agent` (create / restore /
   machine-scoped file ops).
8. **Admin requires ownership.** `owns_agent` is owner-or-`All` only —
   assistant update/install/plan answer `403` to any grantee, however wide the
   grant (`update_target_caps`, pinned by
   `agent_grantee_cannot_administer_assistants`).
9. **Fail-closed ambiguity.** Two visible agents with one label (or two
   machine-less candidates) refuse rather than resolve (pinned by
   `resolve_scoped_refuses_ambiguity_fail_closed`).
10. **Two credentials never cross.** Client credentials (cookie / client token)
    and agent uplink tokens are separate namespaces; each is refused on the
    other's surface (pinned by `client_token_flow_end_to_end`).

## Route inventory

Every route registered in `build_router` (`crates/hub/src/lib.rs`), in
registration order. Columns:

- **auth** — `gated` = behind `require_client_auth` (401 without a session
  cookie / client token in multi-tenant); `exempt(...)` = listed in the
  middleware's exemption set, with why; `own-token` = the agent-uplink gate.
- **agent?** — does handling it reach an agent connection?
- **scoped?** — is that reach through `resolve_scoped` / a `Visibility`
  predicate?
- **scope source** — where the scope/identity comes from.

### Agent uplink (not client-facing, not under `/api/`)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/agent/ws` | GET | own-token (per-agent uplink token → `agents` row; open mode refused in multi-tenant) | registers one | n/a — the token IS the agent's identity; `agent_id`/`user_id` come from the store, never from the frame | uplink token | safe — a leaked client credential cannot register an agent |
| `/agent/bulk` | GET | own-token + slot nonce + machine binding (`BulkRegistry::claim`) | claims one transfer | slot bound to the machine it was opened for; wrong machine → 403, slot retained | uplink token + 256-bit nonce | safe — another agent cannot hijack a transfer |

### Bulk relay (client-facing, streamed to the owning agent)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/download` | GET | gated | yes | `resolve_scoped` before any relay is opened (`bulk::proxy`) | Visibility extension | safe — pinned by `cross_tenant_full_surface_is_refused` |
| `/api/upload` | POST | gated (520 MiB cap) | yes | same | Visibility extension | safe |
| `/api/upload/check` | POST | gated | yes | same | Visibility extension | safe |
| `/api/clip` | POST | gated (32 MiB cap) | yes | same | Visibility extension | safe |
| `/api/clip/targets` | GET | gated | yes | same | Visibility extension | safe |
| `/api/clip/image.png` | GET | gated | yes | same | Visibility extension | safe |

### Aggregation

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/sessions` | GET | gated | reads registry | `all_sessions_for(&scope)` + per-session `lists_session` attribution | Visibility extension | safe |
| `/api/machines` | GET | gated | reads registry | `machines_for(&scope)` (`has_any_visibility`) | Visibility extension | safe |
| `/api/tools` | GET | gated | reads cached tool list | `resolve_scoped`; unresolvable → `200 []`, byte-identical to an unknown machine (deliberate contract — the picker greys out; no info leak, asserted in `cross_tenant_full_surface_is_refused`) | Visibility extension | safe (200-empty, not 404 — documented) |

### Terminal / watch bridges

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/ws` | GET (WS) | gated | yes | `resolve_scoped(scope, machine, Some(session))` **before** `on_upgrade`; the bridged session is the one the scope check ran on | Visibility extension | safe — no 101 cross-tenant |
| `/api/watch` | GET (WS) | gated | yes | `resolve_scoped(scope, machine, None)` before `on_upgrade` | Visibility extension | safe |

### Session lifecycle + control (relayed `Cmd`s)

All of these go through `handlers::route` → `resolve_scoped`; refusal is a
`404` before anything is sent to any agent.

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/session` | POST | gated | yes | `resolve_scoped` (+ per-tenant/pool plan gate first: 402) | Visibility extension | safe |
| `/api/session/delete` | POST | gated | yes | `resolve_scoped` by machine/session | Visibility extension | safe |
| `/api/session/color` | POST | gated | yes | same | Visibility extension | safe |
| `/api/session/label` | POST | gated | yes | same | Visibility extension | safe |
| `/api/session/root` | GET | gated | yes | same | Visibility extension | safe |
| `/api/sessions/restorable` | GET | gated | yes | `resolve_scoped(…, None)` — needs `may_use_agent` (a view-grant can't probe) | Visibility extension | safe |
| `/api/sessions/restore` | POST | gated | yes | same | Visibility extension | safe |
| `/api/key` | POST | gated | yes | `resolve_scoped` by session | Visibility extension | safe |
| `/api/paste` | POST | gated | yes | same | Visibility extension | safe |
| `/api/clear-history` | POST | gated | yes | same | Visibility extension | safe |
| `/api/assistants/update` | GET, POST | gated | yes | `resolve_scoped` **then** `owns_agent` (403 for any grantee) then capability (501) | Visibility extension | safe — owner-only, pinned by `agent_grantee_cannot_administer_assistants` |
| `/api/assistants/plan` | GET | gated | yes | same owner-only gate | Visibility extension | safe — owner-only |

### File browser / editor (relayed small ops)

All via `handlers::file_route` → `route` → `resolve_scoped`.

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/dirs` | GET | gated | yes | `resolve_scoped` (session or machine) | Visibility extension | safe |
| `/api/dirs/search` | GET | gated | yes | same | Visibility extension | safe |
| `/api/files/search` | GET | gated | yes | same | Visibility extension | safe |
| `/api/files` | GET | gated | yes | same | Visibility extension | safe |
| `/api/file/read` | GET | gated | yes | same | Visibility extension | safe |
| `/api/file/write` | POST | gated | yes | same | Visibility extension | safe |
| `/api/file/delete` | POST | gated | yes | same | Visibility extension | safe |
| `/api/mkdir` | POST | gated | yes | same | Visibility extension | safe |
| `/api/rmdir` | POST | gated | yes | same | Visibility extension | safe |
| `/api/rename` | POST | gated | yes | same | Visibility extension | safe |
| `/api/move` | POST | gated | yes | same | Visibility extension | safe |

### Hub-local state (no agent involved)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/favorites` | GET, PUT | gated | no | per-tenant file `config_dir/favorites/<user_id>.json` (single-tenant keeps `favorites.json` byte-for-byte); user id sanitized `[A-Za-z0-9_-]`, unusable id fails closed. A pre-existing shared `favorites.json` on a multi-tenant hub is a mixed-tenant file and is deliberately **not migrated** — it is simply never read in multi-tenant mode | Visibility extension | safe — the 0042 candidate finding #1 fix, pinned by `favorites_are_tenant_scoped` |
| `/api/push/key` | GET | gated | no | public VAPID key, identical for all callers | n/a | safe (public value) |
| `/api/push/subscribe` | POST | gated | no | subscription stamped with the owning tenant (`owner` from the session cookie); `notify_scoped(Some(u))` delivers only to `owner == u` subs. Honest note: a Bearer-authed (ccs) caller has no cookie → `owner: None`, which *receives nothing* in multi-tenant (fail-closed for delivery; push is a browser surface) | cookie (re-derived in handler) | safe |
| `/api/push/unsubscribe` | POST | gated | no | removal keyed by endpoint URL only — the (unguessable) push endpoint is the capability. Accepted residual: no ownership check on removal (deletion-only, no read-back) | n/a | accepted residual (documented) |
| `/api/push/test` | POST | gated | no | buzzes only the caller's own devices (`notify_scoped(owner)`) | cookie (re-derived in handler) | safe |

### Auth / identity (the exemption set — cross-checked against `require_client_auth`)

Every exemption in the middleware, with its justification:

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/login` | POST | exempt (it IS the login; per-source throttle + 250 ms failure delay) | no | mints an identity cookie only for a verified account | credentials in body | safe |
| `/api/auth` | GET | exempt (pre-login gate probe; discloses only `authRequired`/`authed`) | no | n/a | n/a | safe |
| `/api/me` | GET | exempt (the boot "who am I?" — answers `authenticated:false` without a session; account facts only WITH a valid cookie) | no | self only | cookie (re-derived in handler) | safe |
| `/api/logout` | POST | exempt (clearing a cookie needs no session) | no | n/a | n/a | safe |
| `/api/signup` | POST | exempt (it mints the session; per-source throttle, 12-char minimum, no enumeration) | no | creates own account only | n/a | safe |
| `/api/auth/google/start`, `/api/auth/google/callback` | GET | exempt (`/api/auth/google/` prefix — they ARE the login; state+PKCE cookie) | no | identity from Google's token endpoint over validated TLS | OAuth flow | safe |
| `/api/device/code`, `/api/device/token` | POST | exempt (RFC-8628: the `device_code` is the bearer; `slow_down` throttle, single-use) | no | token minted for the approver's tenant only | device code | safe |
| `/api/device/validate` | POST | exempt at the cookie gate — authenticates via the uplink token **in its own handler** (0048) | no | token-scoped | uplink token | safe |
| `/api/device/client/code`, `/api/device/client/token` | POST | exempt (0060 client device flow, same posture as the agent flow; kinds never cross — `device_flow_kinds_never_cross`) | no | token minted for the approver's tenant only | device code | safe |
| `/api/invite/:token` | GET | exempt (the token IS the capability; the landing renders before login; per-source throttle, 404 for dead tokens) | no | token-scoped read | invite token | safe |
| `/api/org-invite/:token` | GET | exempt (same posture, 0063) | no | token-scoped read | invite token | safe |
| `/api/billing/webhook` | POST | exempt (authenticated by the `Stripe-Signature` HMAC in the handler; Stripe sends no Origin) | no | event-scoped | Stripe signature | safe |

`/api/device/approve` is deliberately **not** exempt — it needs the user's
session to bind the enrollment to their tenant.

### Multi-tenant account / dashboard (hub-local, cookie re-derived in handler)

These do not consult the `Visibility` extension; each handler re-derives the
user from the session cookie and every store query is `user_id`-scoped SQL.
(Honest note: Bearer client-token callers get a 401 on these — the account
page is a browser surface.)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/agents` | GET | gated | no | `list_agents(&user_id)` | cookie | safe |
| `/api/agents/unlink` | POST | gated | no | `delete_agent(&user_id, …)` (`AND user_id = ?`) | cookie | safe |
| `/api/agents/rotate` | POST | gated | no | `rotate_agent(&user_id, …)` | cookie | safe |
| `/api/client-tokens` | GET | gated | no | `list_client_tokens(&user_id)` | cookie | safe |
| `/api/client-tokens/delete` | POST | gated | no | owner-scoped delete (foreign id → 404, pinned by `client_token_flow_end_to_end`) | cookie | safe |
| `/api/client-tokens/revoke-self` | POST | gated | no | Bearer self-revoke (the token names itself) | Bearer token | safe |

### Sharing (0039/0040/0056)

Owner/grantee-scoped in the store; a non-party gets 404 (no existence oracle).

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/shares` | POST | gated | no | inviter must own the named machine; uniform known/unknown-email shape (`share_create_does_not_disclose_accounts`); plan-gated (402) | cookie | safe |
| `/api/shares/inbox` | GET | gated | no | grantee-scoped | cookie | safe |
| `/api/shares/outbox` | GET | gated | no | inviter-scoped | cookie | safe |
| `/api/shares/received` | GET | gated | no | grantee-scoped | cookie | safe |
| `/api/shares/received/:id/leave` | POST | gated | no | grantee-scoped | cookie | safe |
| `/api/shares/:id/accept` | POST | gated | no | grantee-only (inviter accepting → 404) | cookie | safe |
| `/api/shares/:id/decline` | POST | gated | no | grantee-only | cookie | safe |
| `/api/shares/:id/revoke` | POST | gated | no | inviter-only | cookie | safe |

### Orgs (0063/0064/0065 — new in this branch)

No `:org_id` in any path: the caller's org is always resolved from their own
membership (`actor_org`), so naming someone else's org is unrepresentable.
Non-members get **404** on every org route; members whose role forbids an
action get **403**. Pinned by `org_routes_are_tenant_scoped`.

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/orgs` | POST | gated | no | creator becomes owner; one-org constraint | cookie | safe |
| `/api/orgs/mine` | GET | gated | no | own membership; no org → 404; pending invites shown to owner/admin only | cookie + membership | safe |
| `/api/orgs/invites` | POST, GET | gated | no | owner/admin of own org; uniform invite shape (no account oracle) | cookie + membership + role | safe |
| `/api/orgs/invites/inbox` | GET | gated | no | by the caller's own account email (membership-free by design — the invitee is not in the org yet) | cookie | safe |
| `/api/orgs/invites/:id/accept` | POST | gated | no | addressee-only (email match, else 404); seat gate 402 | cookie | safe |
| `/api/orgs/invites/:id/decline` | POST | gated | no | addressee-only | cookie | safe |
| `/api/orgs/invites/:id/revoke` | POST | gated | no | owner/admin of the invite's own org (outsider → 404) | cookie + membership + role | safe |
| `/api/orgs/members/:user_id/role` | POST | gated | no | own-org owner only; atomic ownership transfer | cookie + membership + role | safe |
| `/api/orgs/members/:user_id/remove` | POST | gated | no | own-org owner/admin; admin cannot remove owner/admin | cookie + membership + role | safe |
| `/api/orgs/leave` | POST | gated | no | own membership; owner must transfer first (409) | cookie + membership | safe |
| `/api/orgs/machines/:agent_id/visibility` | POST | gated | no | machine **owner** only, whatever their role (no admin override) | cookie + membership + agent ownership | safe |
| `/api/orgs/audit` | GET | gated | no | own-org owner/admin; member → 404-shaped | cookie + membership + role | safe |

### Billing (registered only when `billing::is_configured()`)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/api/billing/checkout` | POST | gated | no | caller's own account/org | cookie | safe |
| `/api/billing/portal` | POST | gated | no | caller's own Stripe customer | cookie | safe |
| `/api/billing/webhook` | POST | exempt (Stripe-Signature HMAC — see exemption table) | no | event-scoped | Stripe signature | safe |

### Public installers + app shell (non-`/api/`, no auth by design)

| route | method | auth | agent? | scoped? | scope source | verdict |
|---|---|---|---|---|---|---|
| `/ccs.sh` | GET | exempt (non-`/api/` install one-liner; static template, no secrets) | no | n/a | n/a | safe |
| `/ccs.ps1` | GET | same | no | n/a | n/a | safe |
| `/install.sh` | GET | exempt (public machine installer; enrollment itself is via the gated device flow) | no | n/a | n/a | safe |
| `/install.ps1` | GET | same | no | n/a | n/a | safe |
| *(fallback)* | GET | exempt (the embedded PWA app shell — static assets only) | no | n/a | n/a | safe |

## How this is tested

- **Registry unit tests** (`crates/hub/src/registry.rs`):
  `resolve_scoped_refuses_ambiguity_fail_closed` (the fail-closed ambiguity
  property — invariant 9), `tenant_scope_isolates_resolve_and_lists` (the §4.1
  keystone), `admin_requires_ownership_not_a_share`,
  `agent_share_visibility_and_asymmetry`, `session_share_is_view_only_and_scoped`,
  `team_grant_is_view_only_machine_wide`, `session_share_back_to_owner`,
  `sessions_count_owned_by_counts_the_pool`.
- **Handlers unit tests** (`crates/hub/src/handlers.rs`):
  `favorites_path_is_scope_keyed` (single-tenant path byte-for-byte, per-tenant
  otherwise), `favorites_path_rejects_unsafe_user_ids`.
- **Two-tenant e2e** (`crates/hub/tests/multi_tenant.rs`, real router + real
  SQLite store + online fake agents):
  `cross_tenant_full_surface_is_refused` (the full Stream A matrix — every
  relayed route × {A's machine label, A's session name, machine-less}, both WS
  bridges, the bulk download proxy, tools-empty parity, machine-list scoping),
  `agent_grantee_cannot_administer_assistants` (invariant 8),
  `org_routes_are_tenant_scoped` (the 0063 surface),
  `favorites_are_tenant_scoped` (the finding-#1 fix),
  plus the pre-existing `tenants_are_isolated_end_to_end`,
  `client_token_flow_end_to_end` (invariant 10),
  `share_create_does_not_disclose_accounts`, `signup_no_enumeration`,
  `signup_throttled`, `password_min_12`, `device_flow_kinds_never_cross`,
  and the sharing-lifecycle tests.
- **Team-tier shell e2e** (`scripts/e2e-team.sh`): stands up a real
  multi-tenant hub binary over loopback and drives the whole
  org/membership/visibility/limits lifecycle over HTTP with SQL row assertions
  (0063 E1 / 0065 Part F).

Run: `cargo test -p cc-screen-hub --features multi-tenant` (the boundary
suites) and `cargo test -p cc-screen-hub` (the single-tenant build stays
byte-for-byte).
