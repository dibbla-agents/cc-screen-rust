# Dibbla CLI — Platform compatibility

What your application must look like to build and run on Dibbla. Use this as the developer-facing mental model for Dockerfile shape, runtime expectations, env vars, and the upload boundary. Cross-links: [reference.md](reference.md) for full CLI flag/syntax detail, [guardrails.md](guardrails.md) for the pre-deploy **security** review (distinct from the compatibility checklist in §12 below).

---

## 1. Scope check — is this even a Dibbla project?

Before applying any of this, confirm at least one Dibbla marker exists in the project:

- A `.dibblaignore` at the deploy root.
- A `dibbla-task.yaml` file (handled by `dibbla run`).
- An `AGENTS.md` or `GEMINI.md` containing the `<!-- >>> dibbla skill >>> -->` markers written by `dibbla skills install`.
- A `.claude/skills/dibbla/` directory in the project.

If none of those are present, this is probably not a Dibbla-connected project — surface that to the user and stop applying Dibbla-specific advice. Generic Dockerfile/security feedback is fine; platform-specific guidance below is not.

---

## 2. Mental model

Your repo is bundled into a tar.gz, your `Dockerfile` is built into a container image, and the image is started behind a public URL at `https://<alias>.dibbla.com`. The platform terminates TLS upstream — your app speaks plain HTTP on whatever port you tell it to listen on. That's the whole shape; everything below is detail.

---

## 3. Dockerfile contract

A `Dockerfile` at the deploy root is **required**. The platform does not run buildpacks or auto-detect languages; if there is no `Dockerfile`, the build fails with `Dockerfile not found in the root of the archive`. Bundled templates in `dibbla-agents/dibbla-public-templates` ship working multi-stage examples — copy a pattern when scaffolding rather than inventing one.

- **Multi-stage builds** keep runtime image size and attack surface small. Build in one stage, copy artifacts into a minimal runtime stage (`node:20-alpine`, `python:3.12-slim`, `gcr.io/distroless/*`, `golang:*` → `gcr.io/distroless/static`).
- **Port matching is mandatory.** Whatever port you `EXPOSE` and listen on inside the container must match the value passed to `dibbla deploy --port <N>`. If you skip `--port`, the platform's default (`80`) is used. So a Node app that `EXPOSE`s `3000` must deploy with `--port 3000` — otherwise traffic is routed to a port nothing is bound to and the public URL hangs or 502s.
- **Listen on `0.0.0.0`, not `127.0.0.1`.** The container's loopback is unreachable from the routing layer; binding to localhost makes the app appear dead.
- **Run as non-root.** The platform does **not** force a non-root user — a Dockerfile that ends up as `root` runs as root. Add `RUN useradd -m app && USER app` (or equivalent in your base image) in the runtime stage.
- **`HEALTHCHECK` is optional but useful.** The platform performs its own post-deploy health check; an in-Dockerfile `HEALTHCHECK` improves debuggability when the platform reports unhealthy.
- **Do not bake secrets in.** `ARG` values and bake-time `ENV` lines land in the image and registry. Use runtime secret injection (§5) instead.

---

## 4. Runtime contract

- **Filesystem is ephemeral.** Anything written to disk is gone on restart, scale-out, or redeploy. Persist via managed Postgres (§7) or external object storage. Do not write user-visible state to local files.
- **Single ingress port.** Only the port matched to `--port` (or `80` by default) is reachable from the public URL. Side-channel ports, sidecar listeners, and direct container-to-container networking are not exposed.
- **Plain HTTP inside the container.** TLS is terminated by the platform's routing layer; do **not** run your own TLS in the container — it'll only see plain HTTP from the proxy.
- **PID 1.** Whatever your `CMD`/`ENTRYPOINT` runs is PID 1 — forward signals correctly, or use a small init shim (`tini`, `dumb-init`) if you spawn children.

---

## 5. Environment variables your app can rely on

User-app environment variables come from three places:

- **Secrets** — `dibbla secrets set <name> <value>` (org-global) or `dibbla secrets set <name> <value> -d <alias>` (scoped to one app). Injected as env vars at runtime. Secret name regex: `^[a-zA-Z][a-zA-Z0-9_]{0,127}$`.
- **`--env KEY=VAL`** on `dibbla deploy` and `dibbla apps update`. Persist across redeploys — set once, they stick.
- **Auto-injected database URL.** `dibbla db create <name>` creates a secret named `DATABASE_URL_<UPPERCASED_UNDERSCORED_NAME>` (e.g. `db create my-db` → `DATABASE_URL_MY_DB`). **There is no plain `DATABASE_URL`** unless you set one explicitly. App code must read the suffixed variable.

Use these channels — never hardcode secrets in the image, in source files committed to VCS, or in `.env` files in the deploy directory (§8 strips them anyway).

---

## 6. Build-time vs runtime env vars (Vite, Next.js, CRA)

This trips up almost every frontend project on first deploy, so understand the distinction before scaffolding.

**The platform's `--env` and secret-injection channels are runtime only.** They set process env vars when the container starts — long after the bundler has finished compiling your JS. Frontend bundlers like **Vite (`VITE_*`)**, **Next.js (`NEXT_PUBLIC_*`)**, and **Create React App (`REACT_APP_*`)** all bake env vars into the JS bundle at **build time**. By the time `--env` arrives, the bundle is frozen — the value will not be there.

**The CLI does not currently expose `--build-arg`.** That means anything that needs to be present during `docker build` has to come from one of these patterns:

1. **Public values → commit them.** Vite/Next/CRA's `*_PUBLIC_*` / `VITE_*` / `REACT_APP_*` prefixes are explicit signals from the framework: *"this value will be visible in browser devtools after the build."* If the value is genuinely public (Supabase anon key, public API URL, feature flags, Sentry DSN), inline it in source or commit a `.env.production` to the repo. **No secret is leaking — it would be in the bundle either way.** Just be sure you're not committing a secret by mistake; the `*_PUBLIC_*` naming convention helps.
2. **Truly secret build-time inputs → don't put them in the bundle.** If a value would be a problem to expose to a logged-in user with devtools open, it doesn't belong in a Vite/Next/CRA build. Move the call server-side: route the request through your backend (read the secret from `dibbla secrets`/`--env` at runtime there) and have the frontend call your backend instead of the third-party API directly.
3. **Per-environment build-time toggles → use a runtime config endpoint.** If you need different values per deploy (staging vs prod) but they aren't *secret*, ship a small `/config.json` endpoint from your backend that returns `{ apiUrl, sentryDsn, ... }` populated from runtime env vars, and have the frontend fetch it on startup. The bundle stays the same across environments; the values come from `--env` at runtime.

What does **not** work:
- Putting `VITE_FOO=...` in `dibbla deploy --env` and expecting the frontend to see it. The bundle was built before that env var existed.
- Relying on a `.env.local` on your laptop. The CLI strips `.env.production` and `.env.prod` and the server denylist strips `.env` and `.env.*` (§8).
- `ARG` directives in the Dockerfile expecting values from `--env` flags. `--env` becomes runtime env, not Docker build args.

Pattern (1) — inlining public values — is the right answer for the Lovable / Supabase / Firebase-frontend genre. Pattern (3) — runtime config endpoint — is the right answer when values genuinely differ across deploys but are still public.

---

## 7. Managed Postgres — self-signed TLS

The Dibbla-managed Postgres serves a **self-signed certificate**. Connections are still encrypted in transit; trust is enforced by network isolation (the database is only reachable from inside the deployment cluster), not CA-rooted cert identity. Two consequences for app code:

- **Do not use `sslmode=disable`.** That drops encryption — wrong fix.
- **Do not require strict CA verification.** A default `sslmode=verify-full` client will refuse the cert. Either strip `sslmode` from the injected URL and configure SSL explicitly with `rejectUnauthorized: false` (Node `pg`, Prisma adapter), or use `sslmode=no-verify` (recent Prisma) or `sslmode=require` with no CA file (psycopg2).

Working snippets for `pg`, Prisma, and psycopg2 live in `reference.md` → "TLS for application database clients" (~line 336). Copy from there rather than inventing a new pattern — naive `ssl: { rejectUnauthorized: false }` is silently shadowed when the URL carries a stricter `sslmode`, which is the most common foot-gun.

---

## 8. Upload boundary — what gets shipped, what gets stripped

Two filtering layers sit between your working tree and the build:

1. **CLI-side exclusion list** strips, before the archive leaves your laptop:
   - SSH keys: `id_rsa`, `id_ed25519`, `id_ecdsa`, `id_dsa`
   - Service-account JSON: `credentials.json`, `service-account.json`
   - Production env files: `.env.production`, `.env.prod`
   - Generic key/cert files: `*.pem`, `*.key`
   - Native binaries: `.exe`, `.dll`, `.so`, `.dylib`, `.bat`, `.cmd`
2. **Server-side denylist** additionally strips `.env`, `.env.*`, `node_modules/`, `dist/`, `.venv/`, `.git/` from the managed VCS history and surfaces each match as a warning in `DeployResponse.vcs_filtered`.

The CLI enforces a **50 MB archive cap** before upload. Server-side per-file and per-commit size caps are hard rejections — exceeding them fails the deploy with `ErrCodeVCSFiltered`.

Use `.dibblaignore` (gitignore syntax, at the deploy root) to silence server-side warnings on intentionally-excluded paths and to keep generated/large artifacts out of Dibbla's managed VCS history. Full syntax in `reference.md` (~line 178). Note: `.dibblaignore` does **not** change what the Docker build sees — it's a VCS filter, not a build-context filter. If you need to keep something out of the image, use a `.dockerignore` as well.

---

## 8.5. Multi-service deployments (`dibbla.yaml`)

When the deploy archive contains a `dibbla.yaml` at the root, the platform applies a **multi-service** path: one deploy bundle ships multiple containers (e.g. `web + worker + redis`) under a single alias, applied atomically with rollback-on-failure. The single-Dockerfile path stays byte-stable for archives without a manifest.

The full schema lives in [manifest.md](manifest.md). This section covers what the *runtime* looks like under multi-service — the env-var contract, the cluster networking shape, and the anti-patterns that the legacy single-Dockerfile habits encourage.

### Runtime contract

Every service container in a multi-service deploy receives a fixed env-var set:

| Variable | Set on every service | Value |
|---|---|---|
| `DIBBLA_DEPLOYMENT_ID` | yes | Stable id across versions of this alias (`dep_<random>`) |
| `DIBBLA_ALIAS` | yes | Deployment alias (`myapp`) |
| `DIBBLA_ENV` | yes | Active env name (`prod`, `staging`, `dev`) |
| `DIBBLA_SERVICE_NAME` | yes | This container's own service name |
| `DIBBLA_SVC_<NAME>_HOST` | one per service in the deploy | Cluster DNS hostname |
| `DIBBLA_SVC_<NAME>_PORT` | one per service that declares `port:` | Container port |
| `DIBBLA_SVC_<NAME>_URL` | one per service that declares `port:` | `http://<host>:<port>` |

`<NAME>` is the service name uppercased with `-` → `_`. Use `${DIBBLA_SVC_*}` substitutions in the `environment:` block of your manifest — the platform substitutes them at render time, before the container starts.

### Cluster networking & exposure

- **Default open within deploy — until `expose_to:` is used anywhere.** When NO service declares `expose_to:`, the deploy is fully permissive: every service can reach every other service over the cluster network. As soon as ANY service declares `expose_to:`, a default-deny NetworkPolicy covers every pod in the deploy (`app: <alias>`) and reachability is gated by explicit allow rules from that point on.
- **There is no per-service "stays open" carve-out** once the switch is flipped. A service without `expose_to:` in a deploy where some other service uses it becomes silently unreachable from siblings. To keep a service callable, either drop `expose_to:` everywhere or declare it on every service that needs to receive traffic.
- **Public traffic is auto-allowed at the ingress edge.** Each `public: true` service gets an automatic allow rule for traffic from the ingress-controller namespace, so `https://<alias>.dibbla.com` keeps serving through the default-deny without any extra declaration. **Internal pod-to-pod calls to a public service are NOT auto-allowed** — if `worker` calls `http://web:3000` over cluster DNS, `web` must also declare `expose_to: [worker]`. NetworkPolicy enforcement requires a CNI that honors it (the platform's clusters do).
- **Internal-only services get no Service object.** A service without `port:` runs as a Deployment but has no K8s Service, so peers can't reach it. `DIBBLA_SVC_<NAME>_HOST` is still set so dependents can name it; `_PORT` and `_URL` are absent.

### Public URL shape

- **One public service:** `https://<alias>.dibbla.com` routes to that service. Backwards-compatible with the legacy single-Dockerfile path.
- **Multiple public services (F14):** the lex-first ("primary") public service serves at `https://<alias>.dibbla.com`; subsequent public services serve at `https://<alias>-<service>.dibbla.com` (one DNS label deep, covered by the existing `*.dibbla.com` wildcard cert). Use `domain:` to claim a custom hostname instead. Per-service auth (`auth.require_login` / `auth.access_policy` / `auth.google_scopes`) is supported and env-aware so a service can be open in dev and locked down in prod with one manifest.
- **Custom domain (`domain:`):** the platform's ingress uses your hostname directly; the alias URL keeps working in parallel. DNS (CNAME to the platform's ingress hostname) is the user's responsibility; cert provisioning is automatic via Let's Encrypt.

### Init containers and healthchecks

- **Init containers run sequentially before the main container.** v1 supports `image:` only (pulled images, no `build:`). Each init must exit cleanly — an init that runs forever blocks the rollout and times out.
- **Healthchecks map to K8s probes 1:1.** Three probe slots (`liveness`, `readiness`, `startup`); each probe is exactly one of `http_get` / `tcp_socket` / `exec`.
- **Skip the field entirely** to use the platform-default health check (the same one the legacy single-Dockerfile path uses).

### What does *not* work in a multi-service deploy

| Habit | What happens |
|---|---|
| `dibbla deploy --cpu 500m --memory 512Mi --port 3000` | These flags are **ignored** when a manifest is present. CPU/memory/port live in the manifest. |
| `dibbla apps update --replicas N` | Returns `PATCH_AMBIGUOUS` (which service?). Edit `dibbla.yaml` and redeploy with `--update` instead. |
| `dibbla apps update --port N` | Same — port is per-service. Edit the manifest. |
| Hard-coding cluster DNS (`http://myapp-redis:6379`) in app source | Brittle across alias renames + breaks if the service name changes. Use `${DIBBLA_SVC_REDIS_URL}` instead. |
| Putting build-time secrets in `--env` | `--env` is runtime-only. Build-time secrets need `build.secrets:` in the manifest + BuildKit `--mount=type=secret`. |
| Expecting `<service>.<alias>.dibbla.com` (subdomain-of-subdomain) for multi-public | Two-label depth requires per-deploy wildcard certs; v1 uses the hyphenated `<alias>-<service>.dibbla.com` scheme that fits the existing `*.dibbla.com` wildcard. |
| `dibbla.yaml` AND `dibbla.yml` both present | `MANIFEST_AMBIGUOUS` — the deploy fails before the upload completes. |

### Resource sums and quotas

The deploy-api runs a quota check on the resolved set BEFORE building anything. Default org quotas: 8 services, 10 replicas/service, 20 replicas total, 8 CPU total, 16Gi memory total, 10Gi PVC/service, 50Gi PVC total. Quota errors surface as `QUOTA_EXCEEDED` with a `path:` like `services.worker.replicas`. See [manifest.md § 18](manifest.md) for the full table.

### Cross-references

- [manifest.md](manifest.md) — schema, env-aware fields, profiles, init, healthcheck, cron, build secrets, custom domains, **stateful services + TCP routes (§ 10.5)**.
- [examples.md](examples.md) — runnable transcripts for each pattern.
- [guardrails.md](guardrails.md) Check 6 — pre-deploy multi-service safety.

---

## 8.6. Stateful services + TCP routes — runtime model

`stateful: true` and per-service `routes:` are the F19 features that let users deploy databases (MongoDB, Redis), brokers (RabbitMQ, NATS), and other wire-protocol services and connect to them from outside the cluster over real TLS. The schema lives in [manifest.md § 10.5](manifest.md); this section covers the runtime — what K8s objects get created, how clients reach them, and the limits operators need to know.

### Workload shape

A service with `stateful: true` renders as:

- An `appsv1.StatefulSet` (instead of the default `Deployment`) with pod-template fields identical to what the Deployment path would produce — same env, image, probes, init containers — so behavior parity is maintained.
- A **headless** `Service` (`ClusterIP: None`) named `<alias>-<svc>-headless`. It has the same selector and ports as the regular Service but no virtual IP; that's what gives each pod stable DNS at `<sts>-<ordinal>.<headless>` (e.g. `myapp-db-0.myapp-db-headless...`).
- The regular `<alias>-<svc>` ClusterIP Service alongside the headless one, for clients that don't care which replica they hit.
- One `volumeClaimTemplate` per declared volume. K8s materializes a per-pod PVC named `<vct-name>-<sts>-<ordinal>` (e.g. `vol-0-myapp-db-0`) on first replica boot.

`replicas > 1` produces N independent pods each with its own PVC. The platform does **not** bootstrap clustering; there is no managed Mongo replica set, no Redis sentinel, no RabbitMQ join. Operators wiring this manually use init containers + headless DNS to discover peers.

### TCP route shape

Each `type: tcp` route renders as a Traefik `IngressRouteTCP` CRD on `traefik.io/v1alpha1`. Traffic flow:

```
client → <hostname>:443 → Traefik LB (TCP entrypoint) → IngressRouteTCP (HostSNI match)
       → ClusterIP Service → StatefulSet pod
```

The TLS choice is per-route:

- `tls: edge` → Traefik terminates TLS using `TraefikTLSCertSecret` (default: `wildcard-tls`). The platform's wildcard cert covers any single-label hostname under the base domain.
- `tls: passthrough` → Traefik does SNI-route-and-forward; the backend pod presents its own cert. Use when the user wants end-to-end TLS or mTLS.

Because routing is **SNI-based on a single shared port (`:443`)**, multiple databases can share that port — the Client Hello SNI value disambiguates. This is exactly why the v1 design only supports TLS-on-connect protocols (Mongo, Redis-TLS, AMQPS, NATS-TLS, Kafka-TLS): protocols that put a TLS Client Hello as the first bytes on the wire are routable; STARTTLS-style protocols (Postgres, MySQL) are not, because their first bytes aren't a TLS handshake. Postgres support requires a Postgres-aware proxy (à la Neon's pgproxy) and is deferred.

### The `:443` port is non-negotiable

Clients connect to the route hostname on **port 443**, not the database's native port. This is the cost of SNI multiplexing — every TCP route on the cluster shares one external port. The container still listens on its native port internally (e.g. Mongo on 27017), but external connection strings target 443:

- `mongodb://my-mongo.dibbla.app:443/?tls=true`
- `redis-cli --tls -h my-cache.dibbla.app -p 443`

Some legacy clients hardcode the protocol's default port. Newer ones (modern MongoDB drivers, redis-cli with `-p`, RabbitMQ ≥ 3.x) accept any port.

### Cloudflare Tunnel cannot carry TCP

Cloudflare Tunnel is HTTP/HTTPS only — it has no concept of L4/TCP forwarding. So when the cluster's `RoutingStrategy` is `cloudflare-tunnel` or `cloudflare-tunnel-ingress`, TCP routes **bypass the tunnel** and use a direct A record:

- HTTP/HTTPS routes still get tunnel ingress rules + CNAME-to-tunnel DNS (legacy behavior, unchanged).
- TCP routes get an A record pointing straight at `INGRESS_HOST_TARGET` (the cluster's TCP-capable LB IP). No Cloudflare in the path.

Operators on tunnel strategies need to ensure `INGRESS_HOST_TARGET` is set to a reachable LB IP. If they want Cloudflare protection for TCP, they need a Cloudflare Spectrum app (paid) — the platform does not stand that up automatically.

### Cluster requirements

- **Traefik as ingress controller.** The TCP route renderer emits `IngressRouteTCP` CRDs (Traefik-specific). Clusters with nginx-ingress or other controllers will see the CRDs go un-applied (the dynamic-client apply is skipped silently). Traefik is the production cluster's choice; alternative-controller support is a follow-up.
- **Default Traefik `TLSStore`** (`traefik.io/v1alpha1` kind `TLSStore`, name `default`, in any namespace Traefik watches) with a `defaultCertificate.secretName` pointing at a wildcard cert that covers `<base-domain>` and one-label subdomains. With this in place, the renderer emits `IngressRouteTCP` resources whose `tls:` block has **no `secretName`** — Traefik falls back to the default cert automatically. This is the recommended configuration because Traefik IngressRouteTCP `tls.secretName` requires the secret in the **same namespace** as the resource, and there's no cross-namespace reference; without a default TLSStore the platform would have to copy a wildcard cert into every tenant namespace on every deploy. If your cluster has no default TLSStore and you instead provision a per-tenant-namespace cert with a known name, set `TRAEFIK_TLS_CERT_SECRET=<name>` and the renderer will reference it explicitly.
- **`TraefikTCPEntrypoint`** (default `websecure`) is the Traefik entrypoint that terminates TCP+TLS. Reusing the standard 443 entrypoint is the v1 default — SNI disambiguates HTTPS vs database traffic on the same port.

If a TCP route is published but Traefik logs `Error configuring TLS error="secret <ns>/<name> does not exist"` repeatedly, the deploy succeeded at the K8s layer but the cert lookup is failing — almost always because the cluster has no default TLSStore AND `TRAEFIK_TLS_CERT_SECRET` was set to a name that doesn't exist in the tenant namespace. The fix is one of: provision a default TLSStore (preferred), unset `TRAEFIK_TLS_CERT_SECRET` so Traefik uses the default, or seed the named secret in every tenant namespace.

### Update semantics — what `--update` can and cannot change

K8s treats most of `StatefulSet.spec` as immutable after create. Allowed: `replicas`, `template` (image, env, resources, probes), `updateStrategy`, `revisionHistoryLimit`, `minReadySeconds`, `persistentVolumeClaimRetentionPolicy`. Forbidden: `volumeClaimTemplates`, `selector`, `serviceName`, `podManagementPolicy`.

Two implications:

1. The platform deliberately renders **stable labels** on `volumeClaimTemplates.metadata` — only the workload-identity set (`app`, `app-name`, `managed-by`, `service`, `role`, `organization-id`). Per-deploy stamps like `deployment-id` are NOT included on VCTs because they would roll on every redeploy and trigger an immutability rejection on something the user didn't actually change. Renderer code: `volumeClaimTemplateLabels()` in `internal/deployer/render/labels.go`.

2. When a deploy DOES touch an immutable field (volume size change, service rename), `applyStatefulSet` wraps the K8s 422 with a clear human-friendly message that points the user at `kubectl delete statefulset <name> --cascade=orphan` followed by a redeploy. The orphan cascade keeps the PVCs intact; the next deploy creates a fresh StatefulSet that adopts them by volumeClaimTemplate-name match.

### Delete semantics

`dibbla apps delete <alias>` removes everything the deploy created — including PVCs and IngressRouteTCP CRDs. There is no "preserve volumes" path in v1. The orchestration:

1. List IngressRouteTCP CRDs by `<base-domain>/app-name=<alias>` label, extract the SNI hostname from each spec, call `RemoveTCPRoute` (which deletes the DNS record), then delete the CRD.
2. Call the legacy `RemoveRoute` (HTTP route DNS cleanup).
3. Label-based delete of K8s objects: Ingresses, Services (both regular and headless), Deployments, StatefulSets, ConfigMaps, **PVCs**.
4. Remove local project files.

The PVC delete step is the destructive part. Tell users to back up first if the data matters.

---

## 9. Public URL & access control

- Default public URL: `https://<alias>.dibbla.com`.
- Gate access with `dibbla deploy --require-login` (any authenticated Dibbla user) plus `--access-policy invite_only` (only explicitly invited users) or `--access-policy all_members` (all org members).
- Request additional Google OAuth scopes (Drive, Calendar, etc.) via `--google-scopes`.
- TLS certificates and routing are managed by the platform — no app-side configuration needed.

---

## 10. Authentication contract — reading the user inside your app

When you deploy with `--require-login`, the platform proxy handles the OAuth flow upstream and only forwards the request to your container after the user is authenticated and authorised by the configured access policy. Your app never sees unauthenticated traffic and never has to validate a JWT itself — by the time a request reaches you, identity has been verified and access policy has been enforced. **You just read headers.**

### The injected headers

The proxy strips any inbound `X-User-*` / `X-Session-Id` headers from the client and replaces them with proxy-set values. Trust them as they arrive — they cannot be spoofed:

| Header | Meaning |
|---|---|
| `X-User-ID` | Stable unique user identifier — use this as the primary key in your DB |
| `X-User-Email` | User's email address |
| `X-User-Name` | Display name |
| `X-User-Org` | Organisation ID the user is acting as |
| `X-User-Org-Role` | Role within that org (e.g. `owner`, `member`) |
| `X-User-Org-Slug` | Organisation slug (URL-safe) |
| `X-User-GlobalAdmin` | `true` if the user is a Dibbla global admin (otherwise absent) |
| `X-Session-Id` | JWT `jti` for browser sessions; empty for API-token requests |

Minimal example (Node/Express):

```js
app.use((req, res, next) => {
  req.user = req.header('x-user-id') ? {
    id:    req.header('x-user-id'),
    email: req.header('x-user-email'),
    name:  req.header('x-user-name'),
    org:   req.header('x-user-org'),
    role:  req.header('x-user-org-role'),
  } : null;
  next();
});
```

Same pattern in any language — read the headers, hydrate a session/user object. **Never** verify, decode, or trust a JWT from the client; there isn't one in the auth contract for app-side use.

### Access policy is proxy-enforced, not app-enforced

`--access-policy invite_only` / `all_members` / `email_domain` is enforced at the proxy. Unauthorised users see a platform-rendered 403 denial page and your container is never reached. Inside your app:

- If the request reached your app, the user is authorised — don't re-check org membership / invite status.
- Do still enforce **per-resource** authorisation in your app (e.g. "this user can view their own todos but not someone else's") using `X-User-ID` / `X-User-Org`.

### Google OAuth scopes (`--google-scopes`)

When the user lacks the requested scopes (e.g. `https://www.googleapis.com/auth/drive.readonly`), the proxy intercepts the request and redirects to a re-consent flow. By the time a request reaches your app with `--google-scopes` set, the user has already granted those scopes.

To **call Google APIs as the user from your backend**, do not try to manage tokens yourself. Call the platform-mediated endpoint pattern:

```
GET /_platform/google/<resource>
```

The proxy forwards these to the platform's Google broker, which retrieves a fresh access token from auth-service and proxies the call. This means **you never see, store, or refresh a Google access token** — refresh is handled transparently. Each call gets a fresh token.

### Logout

If you need to log the user out, redirect them to `/__auth/logout`. The proxy clears the `auth_token` cookie for your app's domain and redirects back to `/`. The user's broader Dibbla portal session is left intact (logout is scoped to your app). Implementing your own session cookies on top of the proxy auth is unnecessary — the proxy cookie is the auth boundary.

### Without `--require-login`

If you do not pass `--require-login`, the proxy injects **no** `X-User-*` headers, even if the user happens to be logged in to Dibbla. This prevents identity leaking into apps that don't expect auth. Either:
- Leave the app fully public, or
- Deploy with `--require-login` so the platform handles auth, or
- Implement your own auth from scratch (cookies/JWT/etc.) — the platform will not interfere.

Mixing modes (e.g. some routes public, others authed) is the app's job — the proxy is binary per deployment.

### Local development

Headers are not magically injected when you run `docker run` locally. Either:

- Mock them with `curl -H "X-User-ID: dev-user" -H "X-User-Email: dev@example.com" ...`, or
- Add a dev-mode bypass in your app (`if process.env.NODE_ENV === 'development' && !req.user) req.user = MOCK_USER`), or
- Run your app behind a local reverse proxy that injects the headers.

Never ship a dev bypass to production. Gate it behind a build flag or env-var check that is impossible to flip on Dibbla (e.g. only honoured when `NODE_ENV !== 'production'`, and ensure you set `NODE_ENV=production` via `--env` at deploy).

---

## 10.5. Workflow function server prerequisites

If you operate workflows that use **file-emitting functions** (`generate_image`, `transcribe_audio`, anything that produces an uploaded artefact via `/api/files/init`), the go-toolserver pod must have `API_TOKEN` set in its environment. The function calls workflow-server's file-init endpoint with that token; without it, the upload step 401s and the agent's reply surfaces as a generic `authentication error` — which is misleading because it's the **storage layer** failing, not the model API. The OpenAI / Whisper / image-model call already succeeded by that point.

| Symptom | What's happening |
|---|---|
| Workflow runs, model API call succeeds, agent reply ends with "authentication error" | go-toolserver lacks `API_TOKEN`; upload to `/api/files/init` returned 401 |
| Same workflow works in one environment, fails in another | The two go-toolserver deployments have different `API_TOKEN` values, or one is missing it |

Set `API_TOKEN` on the go-toolserver deployment as a prerequisite for any workflow that uses file-producing tools. This is independent of any user-app secrets and is **not** something workflow YAML can compensate for.

Cross-reference from [workflows.md](workflows.md) §15 footguns.

---

## 11. What the platform does *not* do for you

The platform is intentionally minimal at the container boundary. It does **not**:

- Force a non-root user (your Dockerfile owns this).
- Mount the root filesystem read-only.
- Run image vulnerability scans.
- Filter outbound network egress.
- Apply request-size limits or rate limits.

These are application-side concerns. For the security review before deploy, run the OWASP-driven checklist in `guardrails.md` — it is distinct from the compatibility checklist in §12.

---

## 12. Compatibility checklist (will it boot and serve traffic?)

**Compatibility, not security.** This list answers "will my app run on Dibbla?" — not "is my app safe to expose?" The security half lives in `guardrails.md` and must be run separately before any deploy.

- [ ] `Dockerfile` at the deploy root.
- [ ] Image `EXPOSE`s and listens on the same port passed to `--port` (or `80` if `--port` is omitted).
- [ ] App binds to `0.0.0.0`, not `127.0.0.1`.
- [ ] `USER` directive sets a non-root user in the runtime stage.
- [ ] All secrets come from `dibbla secrets set` or `--env`, never hardcoded or baked into the image.
- [ ] Postgres client configured to accept the self-signed cert (see §7); `sslmode=disable` is **not** used.
- [ ] For Vite/Next.js/CRA frontends, public values are inlined or fetched via a runtime config endpoint — not passed via `--env` (see §6).
- [ ] If `--require-login`, app reads `X-User-*` headers (see §10) and does not attempt JWT verification.
- [ ] No `.env`, `*.pem`, `*.key`, SSH keys, or `service-account.json` in the deploy directory.
- [ ] `.dibblaignore` covers build outputs and any large generated files.
- [ ] App tolerates an ephemeral local filesystem; persistent state lives in Postgres or external storage.
- [ ] Pre-deploy security review (`guardrails.md`) completed and `REVIEW.md` written before deploying.
