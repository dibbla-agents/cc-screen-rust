# Dibbla CLI — Multi-service manifest (`dibbla.yaml`)

A `dibbla.yaml` lets one deploy bundle multiple services — e.g. a web frontend + a worker + a redis sidecar — under a single alias. The whole graph is built and applied atomically; rollback is automatic on any failure. Use this file as the schema reference when authoring or reviewing a manifest. Cross-links: [reference.md](reference.md) for the CLI flags around manifests, [examples.md](examples.md) for runnable transcripts, [platform.md § 8.5](platform.md) for the runtime contract under multi-service, [guardrails.md](guardrails.md) for pre-deploy multi-service safety checks.

---

## 1. When to use a manifest

Add a `dibbla.yaml` when **any** of these is true:

- The app needs more than one container — e.g. `web + worker`, `api + cache`, `web + redis + worker`.
- The app needs a per-environment shape — e.g. `replicas: 1` in dev, `replicas: 3` in prod, sourced from one file.
- The app needs init steps that run before the main container — e.g. db migrations.
- The app needs cron jobs declared alongside the deployment.
- The app needs a per-service secret (a token only the worker should see) or a build-time secret (private npm token used during `docker build`).

Keep the legacy single-`Dockerfile` path when:

- The app is one container and stays that way.
- All env vars are happy living on `dibbla deploy --env` / `dibbla apps update --env`.
- You don't need cross-service discovery (`DIBBLA_SVC_*` env vars).

The two paths are mutually exclusive at deploy time: detection is `dibbla.yaml` present at the root of the archive ⇒ multi-service; absent ⇒ single-Dockerfile. The CLI fails fast if `dibbla.yaml` AND `dibbla.yml` are both present (`MANIFEST_AMBIGUOUS`) so you don't have to guess which one ships.

> **Local iteration.** The platform does not run `dibbla.yaml` locally — there is no `dibbla up`. `dibbla preview` is a server-authoritative dry run, not a local stack. For tight inner-loop development (edit code → see it live in seconds) mirror the manifest into a `docker-compose.yml` next to it. The two formats diverge in details (no `${DIBBLA_SVC_*}` substitution locally, no NetworkPolicy enforcement, no env-aware/profile resolution) but match in shape — same containers, same ports, same env vars, same volumes. See [examples.md](examples.md) "Local iteration with docker-compose" for a side-by-side mapping cheat-sheet.

---

## 2. Mental model

Think of the manifest as a typed declaration of a small Kubernetes graph. Each `services:` entry becomes a Deployment + Service (and optionally PVCs + NetworkPolicy + Ingress). Each `jobs:` entry becomes a CronJob. The deploy-api:

1. Parses + validates the manifest.
2. Resolves env-aware fields against the active env (`prod` by default).
3. Filters services by active `profiles:`.
4. Runs a quota check on the resolved set.
5. Builds every `build:`-typed service in parallel via BuildKit.
6. Renders the final K8s objects with stable names and labels.
7. Applies them with rollback-on-failure (a journal of pre-state means a partial failure rolls every object back to where it started).
8. Marks orphans (objects from the previous deploy that no longer exist) for sweep.
9. Sets up the public route to the one service that has `public: true`.

Local validation (`dibbla manifest validate`) runs steps 1 only. Server preview (`dibbla preview`) runs 1–4. A real deploy runs 1–9.

---

## 3. Top-level shape

```yaml
version: 1                       # required; only 1 is supported today
services:                        # required; at least one entry
  web:
    build: ./web
    port: 3000
    public: true
  worker:
    build: ./worker
  redis:
    image: redis:7
    port: 6379
jobs:                            # optional; cron-style scheduled jobs
  nightly-cleanup:
    schedule: "0 2 * * *"
    image: alpine:3.20
    command: [sh, -c, "echo cleanup"]
```

Reserved top-level keys (rejected today, kept for future versions): `volumes:`, `networks:`, `secrets:`, `cron:`, `init:`. Use the per-service equivalents instead.

---

## 4. Per-service fields

| Field | Type | Required | Env-aware | Default | Notes |
|---|---|---|---|---|---|
| `build` | string \| object | one of build/image | no | — | Path or build spec; see § 5 |
| `image` | string \| env-map | one of build/image | yes | — | Pulled image; must include a tag |
| `port` | int | when `public: true` | no | — | 1–65535; container port the service listens on |
| `public` | bool \| env-map | no | yes | `false` | Routes a public URL to this service; multiple `public: true` services per deploy supported (see § 13) |
| `replicas` | int \| env-map | no | yes | `1` | Number of pods; capped by org quota (§ 19) |
| `cpu` | string \| env-map | no | yes | platform default | K8s CPU spec (`500m`, `1`, `2`); resource quota applies |
| `memory` | string \| env-map | no | yes | platform default | K8s memory spec (`256Mi`, `1Gi`) |
| `environment` | string-map \| env-map-of-string-maps | no | yes | empty | Env vars; supports `${VAR}` substitution (§ 8) |
| `command` | list-of-strings | no | no | image default | Overrides the container `CMD` |
| `entrypoint` | list-of-strings | no | no | image default | Overrides the container `ENTRYPOINT` |
| `volumes` | list of `{path, size, access?}` | no | no | empty | Per-service PVCs; see § 10 |
| `stateful` | bool | no | no | `false` | Render as StatefulSet + headless Service; requires at least one volume. See § 10.5 |
| `routes` | list of `{type, port, tls?, hostname?}` | no | no | empty | Externally-routable endpoints. `type: tcp` exposes raw TCP with TLS at the edge; `type: https`/`http` produces a standard Ingress. See § 10.5 |
| `profiles` | list-of-strings | no | no | empty | Activation gates; see § 7 |
| `depends_on` | list-of-strings | no | no | empty | Boot ordering hint; rolls out in topological order. References resolve against the manifest, not the post-profile-filter set — so `depends_on: [dep]` with `dep` profile-gated stays valid in all envs; in envs where `dep` is filtered out the hint is a no-op (no error, no boot ordering). For cross-profile dependencies, prefer application-level retry over `depends_on`. |
| `expose_to` | list-of-strings | no | no | open | NetworkPolicy whitelist; see § 9 |
| `init` | list of init containers | no | no | empty | Run-once containers before main; see § 11 |
| `healthcheck` | object | no | no | platform default | Liveness/readiness/startup probes; see § 12 |
| `domain` | string | no | no | `<alias>.dibbla.com` | Custom hostname for the public service; see § 14 |
| `auth` | object | no | yes (per-field) | fall back to deploy flags | Per-service auth policy (`require_login`, `access_policy`, `google_scopes`); see § 13 |

Service names must match `^[a-z][a-z0-9-]{0,29}$`. Reserved names: `proxy`, `auth`, `system`, `dibbla`, `kube-*`. Service names appear in DNS, in env-var names (`DIBBLA_SVC_<NAME>_HOST` becomes upper-case-with-`_`), and in K8s object names — keep them short and DNS-safe.

---

## 5. Build vs image

Every service carries either `build:` (Dockerfile-based) or `image:` (pulled image), never both. The validator emits `MANIFEST_INVALID` if both or neither are set.

### `build:` shorthand (string)

```yaml
services:
  web:
    build: ./web        # context dir; defaults Dockerfile to ./web/Dockerfile
```

### `build:` object form

```yaml
services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile.prod      # defaults to "Dockerfile"
      target: runtime                  # optional multi-stage target
      args:                            # docker build --build-arg
        NODE_VERSION: "20"
        PUBLIC_API_URL: "https://api.example.com"
      secrets:                         # build-time secrets; see § 16
        - id: npm_token
          source: NPM_TOKEN_SECRET
```

Every `build.context` must exist inside the archive. Missing context fails the deploy with `BUILD_CONTEXT_MISSING`.

### `image:` (pulled)

```yaml
services:
  redis:
    image: redis:7
    port: 6379
```

Image refs **must include a tag**. `image: redis` is rejected (`MANIFEST_INVALID`); use `redis:7` or `redis:latest` (and don't use `latest` in prod — pin a digest).

The platform allows pulls from a configured registry allowlist (Docker Hub, GHCR, GCR by default). Private images require a build-time secret or pre-pushed image to the org registry; ask the platform operator if you need a new registry whitelisted.

---

## 6. Env-aware fields

Every field marked **Env-aware** in § 4 accepts either a scalar (uniform across environments) or a per-environment map.

### Scalar form

```yaml
services:
  web:
    replicas: 2          # always 2, regardless of --target-env
```

### Per-environment form

```yaml
services:
  web:
    replicas:
      default: 1         # used when no env-specific key matches
      staging: 2
      prod: 5
```

Resolution order at deploy time:

1. If the field is a scalar, use it.
2. Otherwise, look up `--target-env` in the map.
3. If absent, fall back to `default:`.
4. If `default:` is also absent, fall back to the field's documented default (§ 4).

The active env name is whatever `--target-env` is set to; **the server defaults to `prod`** when the flag is omitted. Resolution happens server-side so the local `dibbla manifest validate` does NOT exercise this — use `dibbla preview --target-env <env>` for that.

The `environment` field uses a richer form: per-env values are themselves maps of `KEY: value`, and `default:` overlays under the env-specific entries.

```yaml
services:
  web:
    environment:
      default:
        LOG_LEVEL: info
        FEATURE_FLAG_X: "false"
      prod:
        FEATURE_FLAG_X: "true"        # overlays default
        SENTRY_DSN: "https://..."     # only in prod
```

Resolved environment for `--target-env prod`:
```
LOG_LEVEL=info               (from default)
FEATURE_FLAG_X=true          (prod overrides default)
SENTRY_DSN=https://...       (prod-only)
```

You can mix flat and per-env forms across fields, but **not within one field**. The validator rejects `environment:` with mixed scalar and mapping values to keep resolution unambiguous.

---

## 7. Profiles

Profiles activate or skip whole services without changing the YAML. Use them when a service exists in some environments and not others (e.g. `mailcatcher` in dev, omitted in prod).

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
  mailcatcher:
    image: mailhog/mailhog:v1.0.1
    port: 8025
    profiles: [dev]                  # only deployed when --profile dev is passed
  metrics-shipper:
    image: prom/prometheus:v2.50
    port: 9090
    profiles: [observability]
```

Activation is additive: `dibbla deploy --profile dev --profile observability` activates both. A service with no `profiles:` is always active. Skipped services appear in `PreviewResponse.skipped_services` with the reason.

**Profiles vs env-aware:** profiles toggle whether a service exists at all; env-aware fields shape an existing service. If you want one less service in prod, use a profile. If you want the same service with `replicas: 1` in dev and `replicas: 5` in prod, use env-aware `replicas:`.

### Worked example: inline DB in dev, external in prod

The canonical multi-env pattern for stateful apps — run a DB container alongside in dev for fast iteration, point at managed/external storage in prod. Three pieces working together: a profiled inline service, env-aware connection strings on the consumer, and (optionally) `depends_on` for boot ordering in dev.

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
    environment:
      default:
        # In non-dev, MONGO_URL must come from somewhere outside the deploy:
        #   — `dibbla apps update <alias> -e MONGO_URL=...`,
        #   — a deployment secret, or
        #   — shell-substituted from CI: `MONGO_URL=... dibbla deploy ...`.
        MONGO_URL: ${MONGO_URL}
      dev:
        # Service-discovery vars only resolve when `mongo` is in the active deploy.
        MONGO_URL: mongodb://${DIBBLA_SVC_MONGO_HOST}:${DIBBLA_SVC_MONGO_PORT}/

  mongo:
    image: mongo:7
    port: 27017
    profiles: [dev]                    # only included when --profile dev is passed
    expose_to: [web]
    volumes:
      - { path: /data/db, size: 1Gi }
```

Deploy commands:

```bash
# Dev — inline mongo container is part of the deploy, web reads ${DIBBLA_SVC_MONGO_*}
dibbla deploy --target-env dev --profile dev -m "feat: ..."

# Prod — mongo service is filtered out, web reads MONGO_URL from a managed source
MONGO_URL=mongodb+srv://... dibbla deploy --target-env prod -m "feat: ..."
```

Three things to know for this pattern:

1. **`${DIBBLA_SVC_MONGO_*}` only resolves when `mongo` is in the active deploy.** That's why the `default:` branch above can't reuse those vars — they don't exist when mongo is profiled out. Use a different value source (managed-DB connection string, secret, shell var) for non-dev.
2. **`depends_on` references are not env-filtered.** `depends_on: [mongo]` would stay valid in prod even though mongo is gone — at runtime the hint is just dropped (no `DEPENDS_ON_UNKNOWN`). Best practice for cross-profile deps: omit `depends_on` and rely on application-level retry (PyMongo reconnect, libpq retry, etc.) for connection robustness.
3. **`--target-env dev` and `--profile dev` are independent flags.** The first selects the env-aware `dev:` branch; the second activates `profiles: [dev]` services. You almost always want both together for the dev variant — combine them in your dev deploy command (or wrap in a `make dev-deploy` target so you don't have to remember).

For local iteration on this same shape (no Dibbla cluster involved), see [examples.md](examples.md) "Local iteration with docker-compose".

---

## 8. Service discovery contract

When the deploy lands, every container in the deployment receives a fixed set of env vars so services can reach each other and identify themselves.

| Variable | Set on every service | Value |
|---|---|---|
| `DIBBLA_DEPLOYMENT_ID` | yes | Stable id across versions of this alias (`dep_<random>`) |
| `DIBBLA_ALIAS` | yes | Deployment alias (`myapp`) |
| `DIBBLA_ENV` | yes | Active env name (`prod`, `staging`, `dev`) |
| `DIBBLA_SERVICE_NAME` | yes | This container's own service name |
| `DIBBLA_SVC_<NAME>_HOST` | one per service in the deploy | Cluster DNS hostname |
| `DIBBLA_SVC_<NAME>_PORT` | one per service that declares `port:` | Container port |
| `DIBBLA_SVC_<NAME>_URL` | one per service that declares `port:` | `http://<host>:<port>` |

`<NAME>` is the service name uppercased with `-` → `_`. So a service `my-worker` becomes `DIBBLA_SVC_MY_WORKER_HOST` etc.

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
    environment:
      REDIS_URL: ${DIBBLA_SVC_REDIS_URL}        # substituted at deploy time
      WORKER_HOST: ${DIBBLA_SVC_WORKER_HOST}
  worker:
    build: ./worker
  redis:
    image: redis:7
    port: 6379
```

Inside `web`, `REDIS_URL` resolves to e.g. `http://myapp-redis:6379`. The substitution happens server-side during render — the value is literal in the env after that. **Hard-coding cluster DNS bypasses the contract** and breaks across alias renames; use `${DIBBLA_SVC_*}` instead.

A service with no `port:` gets a Deployment but no Service object — it's reachable only via in-pod loopback (and probably shouldn't be reached at all). `DIBBLA_SVC_<NAME>_HOST` is still set so dependents can know it's part of the graph; `_PORT` and `_URL` are absent.

---

## 9. `expose_to:` and NetworkPolicy

`expose_to:` is a **deploy-level switch**, not a per-service toggle. The moment ANY service in the active set declares it, the platform emits a default-deny NetworkPolicy covering every pod in the deploy (`app: <alias>`). After that, peers can only reach a service through an explicit allow rule.

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
  worker:
    build: ./worker
    port: 9090
    expose_to: [web]                  # only web can reach worker:9090
  redis:
    image: redis:7
    port: 6379
    expose_to: [web, worker]          # both can talk to redis
```

Effects:

- **No service uses `expose_to:` → no NetworkPolicies are emitted** and the deploy stays fully permissive (every service reaches every other service via cluster DNS).
- **Any service uses `expose_to:` → default-deny applies to every pod in the deploy.** A service that does NOT declare `expose_to:` is then *silently unreachable* from siblings — there is no "stays open" carve-out for individual services once the switch is flipped.
- **Public services keep their public URL.** The renderer auto-emits an allow rule for each `public: true` service that admits traffic from the ingress-controller namespace, so `https://<alias>.dibbla.com` keeps working through the default-deny without any extra declaration on the public service.
- **Pod-to-pod traffic to a public service is still gated.** If `worker` calls `http://web:3000/` directly via cluster DNS, the call is denied unless `web` also declares `expose_to: [worker]`. The auto-allow only covers the ingress controller, not internal siblings.
- **References are validated.** A service named in `expose_to:` must exist in the manifest; unknown peers fail with `MANIFEST_INVALID`.

A practical pattern: if you need internal segmentation (e.g. lock down a database), declare `expose_to:` on the segmented service AND on every public service that you also want callable from internal siblings. If you don't need segmentation, leave `expose_to:` off everywhere — the open default is far simpler.

NetworkPolicy enforcement requires a CNI that honors it (Calico, Cilium, etc.). The platform's clusters use such a CNI; outside that you'll get the policies as plain YAML with no enforcement.

---

## 10. Volumes

Volumes attach a per-service PersistentVolumeClaim. Mounts are inside the container; the PVC lifecycle follows the deploy.

```yaml
services:
  redis:
    image: redis:7
    port: 6379
    volumes:
      - path: /data
        size: 1Gi
        access: rwo                   # default; omit for ReadWriteOnce
  shared-tools:
    image: alpine:3.20
    volumes:
      - path: /shared
        size: 5Gi
        access: rwx                   # ReadWriteMany — multi-pod write
```

Field rules:

- `path` is required, must start with `/`.
- `size` is required, K8s storage shape (`1Gi`, `500Mi`, `10Gi`). Per-service quota: 10Gi default; per-deploy total: 50Gi default.
- `access` defaults to `rwo` (`ReadWriteOnce`). Use `rwx` only when you genuinely need multi-pod write — most clusters charge more for that storage class.
- PVC name: `<resource>-<service>-<index>` (e.g. `myapp-redis-0`).
- Lifecycle: created on first deploy, retained across redeploys, **deleted with the deployment**. There is no "keep PVC across delete" knob in v1 — back up before `dibbla apps delete`.

Reserved top-level `volumes:` (for shared/named PVCs) is parsed but rejected today (`MANIFEST_UNSUPPORTED`). Per-service volumes are the only supported form.

---

## 10.5. Stateful services + TCP routes

Two paired features for running databases (MongoDB, Redis), message brokers (RabbitMQ, NATS), and similar wire-protocol services on Dibbla — and connecting to them from outside the cluster over real TLS.

### `stateful: true`

Setting `stateful: true` on a service changes the workload shape from `Deployment` (the default) to `StatefulSet`, and renders a paired headless Service so each pod has stable per-replica DNS. Per-replica PVCs come from `volumeClaimTemplates` — each replica gets its **own** independent volume, materialized as `<vol-name>-<sts>-<ordinal>` (e.g. `vol-0-myapp-db-0`).

```yaml
services:
  db:
    image: mongo:7
    port: 27017
    stateful: true
    volumes:
      - path: /data/db
        size: 10Gi
```

Field rules:

- `stateful: true` requires at least one entry in `volumes:` — fails validation with `STATEFUL_NO_VOLUME` otherwise. The whole point of stateful is durable storage.
- Pod DNS: `<sts>-<ordinal>.<sts>-headless.<ns>.svc.cluster.local` (e.g. `myapp-db-0.myapp-db-headless...`). The regular ClusterIP Service at `<alias>-<svc>` also exists for clients that don't care which replica they hit.
- StatefulSet pods restart in ordinal order — a stuck pod blocks the rest. Tune healthchecks tighter than for stateless workloads.

**`replicas > 1` on a stateful service is allowed but produces N independent pods, each with its own PVC and its own data set.** The platform does **not** bootstrap clustering protocols (Mongo replica set initiation, Redis sentinel/cluster, RabbitMQ join_cluster, etc.) — you'd need to wire that up yourself with init containers + your own config. For most use cases stick with `replicas: 1` unless you have a clustering recipe ready. Managed-cluster recipes per engine are a follow-up.

### `routes:` — externally-routable endpoints

A `routes:` list per service declares how clients outside the cluster reach the service. Each route is one hostname:port pair with a TLS strategy:

```yaml
services:
  db:
    image: mongo:7
    port: 27017
    stateful: true
    volumes:
      - path: /data/db
        size: 10Gi
    routes:
      - type: tcp
        port: 27017
        tls: edge                  # platform-managed wildcard cert
        hostname: my-mongo         # → my-mongo.<base-domain>:443
```

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `type` | enum | yes | — | `tcp` \| `https` \| `http` |
| `port` | int | yes | — | Container port the route forwards to |
| `tls` | enum | no | see below | `edge` \| `passthrough` \| `none` |
| `hostname` | string | no | `<alias>-<svc>` | Subdomain label (DNS-label rules); the renderer appends the base domain |

`tls` defaults to `edge` for `tcp`/`https` and `none` for `http`. Reserved combinations:

- `type: http` + `tls: edge` → `ROUTE_INVALID` (use `type: https` for managed TLS)
- `type: tcp` + `tls: none` → `ROUTE_INVALID` (no way to disambiguate without TLS SNI on shared :443)
- `hostname:` must match `^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$` (DNS label, max 63 chars)

### TCP route mechanics — and why Postgres/MySQL don't work yet

`type: tcp` routes are realized as Traefik `IngressRouteTCP` CRDs that route by **SNI** on a single shared port (`:443`). The platform inspects the TLS Client Hello, reads the SNI hostname, and forwards to the right backend Service. With `tls: edge` the platform terminates TLS using the cluster's default `TLSStore` certificate (a wildcard provisioned in the ingress namespace by the operator); with `tls: passthrough` the server presents its own cert and the platform just passes bytes through.

A note on cluster setup: Traefik IngressRouteTCP requires the TLS cert secret to live in the **same namespace** as the route, and there's no cross-namespace reference — so the platform leans on the default TLSStore (cluster-wide) rather than copying a wildcard secret into every tenant namespace. If a TCP route deploys cleanly but the connection refuses TLS, check that the cluster has a default TLSStore configured (`kubectl get tlsstore -A` should show one named `default` with a `defaultCertificate.secretName`). Operators on clusters without a default TLSStore can set `TRAEFIK_TLS_CERT_SECRET` to a known per-tenant cert name; without either, Traefik will log `Error configuring TLS ... secret does not exist` every 10s and the route won't serve.

This works for **TLS-on-connect protocols** — protocols where the very first bytes on the wire are a TLS Client Hello:

- MongoDB (with `tls: true`)
- Redis with TLS
- RabbitMQ AMQPS (port 5671)
- NATS with TLS
- Kafka with TLS
- Anything else that does TLS handshake first, application protocol second

It does **not** work for **STARTTLS-style protocols** that send plaintext bytes before upgrading:

- **Postgres** — the client first sends an 8-byte plaintext `SSLRequest`, the server responds `S` or `N`, *then* TLS begins. SNI routing on `:443` can't read those first 8 bytes as a TLS Client Hello, so it can't pick a backend. Routing Postgres requires a Postgres-aware proxy (Neon's pgproxy is the reference design) or a dedicated port per instance.
- **MySQL / MariaDB** — same shape: server greets first, client replies with capability flags including a "want SSL" bit, *then* TLS upgrades.

If a user asks for a TCP route to Postgres or MySQL, the platform can render the manifest but the connection won't work. Tell them to use the managed `dibbla db` subsystem for Postgres, and that engine-aware TCP routes for STARTTLS protocols are a follow-up.

### Cloudflare Tunnel bypass

When the platform's routing strategy is `cloudflare-tunnel` or `cloudflare-tunnel-ingress`, HTTP traffic flows through the tunnel as usual. **TCP routes bypass the tunnel** because Cloudflare Tunnel is HTTP-only — they get a direct A record straight to the cluster's TCP load balancer (`INGRESS_HOST_TARGET`). The user-visible consequence: a TCP route's hostname resolves to a different IP than the deployment's HTTPS hostname; the TCP route is **not** behind Cloudflare's edge protection unless the operator stands up Cloudflare Spectrum separately.

### Connecting from your laptop

After a successful deploy, the CLI prints a connection-info line per TCP route. Engine-agnostic shape: `tcp+tls://<hostname>:443`. Translate to your engine's URI scheme as needed:

```bash
# MongoDB on port 27017 → my-mongo.dibbla.app:443 over TLS
mongosh "mongodb://my-mongo.dibbla.app:443/?tls=true"

# Redis-with-TLS on port 6379 → my-cache.dibbla.app:443
redis-cli --tls -h my-cache.dibbla.app -p 443

# RabbitMQ AMQPS on port 5671 → my-broker.dibbla.app:443
rabbitmqadmin --ssl --host my-broker.dibbla.app --port 443 ...
```

Note that the **connection port from the laptop is 443**, not the protocol's native port — SNI-based multiplexing on a single port is the whole point. The container still listens on its native port internally; only the externally-visible port is rewritten to 443.

### Updating a stateful service — what's mutable, what isn't

Kubernetes treats most of a `StatefulSet`'s spec as **immutable after creation**. You can update:

- `replicas` (scale up/down)
- `template` (the pod spec — image tag, env, healthcheck, resources)
- `updateStrategy`, `revisionHistoryLimit`, `minReadySeconds`, `persistentVolumeClaimRetentionPolicy`

You **cannot** in-place update:

- `volumeClaimTemplates` (so resizing a `volumes:` entry, or adding a new one, is not a `--update` operation)
- `selector`, `serviceName`, `podManagementPolicy`

Routine `dibbla deploy --update` works for what you'd actually want most of the time: rolling out a new image, changing env vars, tuning resources. If you need to change a field K8s won't let you mutate, the deploy errors out with a clear message that names the immutable categories and points at the recovery procedure:

```
StatefulSet "myapp-db" rejected: a change in this deploy touches an immutable field
on the existing StatefulSet (volumeClaimTemplates, selector, serviceName, or
podManagementPolicy). To apply such a change you must delete the StatefulSet and
let the next deploy recreate it; preserve the underlying PVCs by deleting the
StatefulSet with --cascade=orphan first (e.g. `kubectl delete statefulset myapp-db
--cascade=orphan -n <ns>`), then redeploy. The new StatefulSet will adopt the
existing PVCs because their names match the volumeClaimTemplates.
```

The cascade-orphan trick keeps the PVCs alive (and the data on them) while letting K8s recreate a fresh StatefulSet that picks up the new spec. The volumeClaimTemplate names are stable across deploys (the platform doesn't churn them), so adoption Just Works.

### Delete is destructive

`dibbla apps delete <alias>` on a deployment that includes stateful services removes:

- The StatefulSet (and its pods)
- The headless + regular Services
- The IngressRouteTCP CRDs
- The TCP route DNS records
- **The PVCs and all their data**
- Any HTTP Ingresses, ConfigMaps, NetworkPolicies, etc. (the rest of the legacy delete path)

There is **no `--preserve-volumes` flag in v1**. If you might want the data, back it up before deleting (database-engine native dump → S3, etc.). Engine-aware backups in the manifest itself are a deferred feature.

### Validation error codes

| Code | Meaning |
|---|---|
| `STATEFUL_NO_VOLUME` | `stateful: true` set but no `volumes:` declared |
| `ROUTE_INVALID` | One of: bad `type`, bad `tls`, port out of range, invalid hostname, illegal type/tls combination |

### Worked example — MongoDB exposed to your laptop

```yaml
version: 1
services:
  db:
    image: mongo:7
    port: 27017
    stateful: true
    replicas: 1
    cpu: 500m
    memory: 1Gi
    volumes:
      - path: /data/db
        size: 10Gi
    healthcheck:
      readiness:
        tcp_socket: { port: 27017 }
        period_seconds: 5
    routes:
      - type: tcp
        port: 27017
        tls: edge
        hostname: my-mongo
```

After `dibbla deploy --target-env prod`, the CLI prints `tcp+tls://my-mongo.dibbla.app:443`. Connect with `mongosh "mongodb://my-mongo.dibbla.app:443/?tls=true"`.

---

## 11. Init containers

Init containers run sequentially before the main container starts. They're for migrations, schema sync, asset hydration — anything that must finish before the main process is healthy.

```yaml
services:
  api:
    build: ./api
    port: 8080
    public: true
    init:
      - name: migrate
        image: registry.example.com/migrate:v1
        command: [/usr/bin/migrate, "up"]
        environment:
          DATABASE_URL: ${DIBBLA_SVC_REDIS_URL}     # service-discovery substitution works
      - name: seed
        image: registry.example.com/seeder:v1
        command: [seeder, "--once"]
```

Rules:

- Init entries are an ordered list — they run in order, all must succeed before the main container starts.
- v1 supports `image:` only — no `build:` for init containers. The container has to be a pre-built pulled image. Use a build step in your CI to produce one if you need code from this repo.
- Each init container must **exit cleanly**. An init that runs forever blocks the rollout and the deploy will time out.
- `environment:` here is a flat map (no per-env form); use a single map of literals or `${VAR}` substitutions.
- `name:` must be unique within the service and DNS-safe (matches `^[a-z][a-z0-9-]{0,29}$`).

Init containers count against the deploy's pod resource budget — the cluster needs to schedule them too. They share the pod's PVCs (so an init can write fixtures into a `/data` mount the main container then reads).

---

## 12. Healthchecks

Healthchecks are per-service liveness/readiness/startup probes. Maps to K8s `*corev1.Probe` 1:1.

```yaml
services:
  api:
    build: ./api
    port: 8080
    public: true
    healthcheck:
      liveness:
        http_get: { path: /healthz, port: 8080 }
        initial_delay_seconds: 10
        period_seconds: 5
        timeout_seconds: 2
        failure_threshold: 3
      readiness:
        http_get: { path: /ready }      # port defaults to service.port
        period_seconds: 5
      startup:
        tcp_socket: { port: 8080 }
        failure_threshold: 30           # 30 * period_seconds before liveness kicks in
```

Probe variants:

- `http_get: { path, port? }` — HTTP GET that must return 2xx/3xx. `port` defaults to `service.port`.
- `tcp_socket: { port }` — port must be open.
- `exec: ["cmd", "arg1", ...]` — exec inside the container; exit 0 = healthy.

Exactly one of `http_get` / `tcp_socket` / `exec` per probe (validator enforces). The three probe slots (`liveness`, `readiness`, `startup`) are independent — set what you need.

Common rules:

- `initial_delay_seconds` defaults to platform-sane values; bump for slow-starting JVMs.
- `failure_threshold` ≥ 3 in production keeps a single transient failure from killing the pod.
- `startup` is the right home for slow boots: it disables liveness until the startup probe passes, so a 60-second cold start doesn't get killed at second 10.

Skip the field entirely to use the platform default health check, which is the same one the legacy single-Dockerfile path uses.

---

## 13. Multiple public services + per-service auth

A deploy can mark more than one service `public: true`. Each gets its own URL using the **hyphenated subdomain scheme** so the platform's existing single-label wildcard cert (`*.dibbla.com`) covers every public service without per-deploy wildcard issuance.

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true                       # https://myapp.dibbla.com
  api:
    build: ./api
    port: 8080
    public: true                       # https://myapp-api.dibbla.com
  pgadmin:
    image: dpage/pgadmin4:latest
    port: 80
    public: true                       # https://myapp-pgadmin.dibbla.com
    auth:
      require_login: true
      access_policy: invite_only
```

URL shape:

- **Primary** (lex-first public service in the manifest's active set): `https://<alias>.<base-domain>` — the bare alias. Backwards compatible with single-public deploys.
- **Secondary** public services: `https://<alias>-<service>.<base-domain>`. One DNS label deep, so the existing wildcard cert covers it.
- **Custom domain** override: a service with `domain: api.example.com` claims that hostname instead. User owns the DNS (CNAME to the platform's ingress hostname); platform issues a Let's Encrypt cert. See § 14.

**Hostname-collision check.** If your alias plus a service name would shadow a different existing alias (e.g. you deploy `myapp` with a `web` public service while alias `myapp-web` already exists in the same org), the deploy fails with `ALIAS_HOSTNAME_COLLISION` before any side effects. Rename either deploy.

### Per-service auth policy

Today's deploy-level `--require-login` / `--access-policy` / `--google-scopes` flags apply to every public service in the deploy. With multi-public, that's almost never what you want — the canonical case is open-`web` + login-gated `pgadmin`. Use the per-service `auth:` block instead:

```yaml
services:
  web:
    public: true                       # open to anyone
  pgadmin:
    public: true
    auth:
      require_login: true
      access_policy: invite_only       # only invited users
      google_scopes: []                # optional, list-of-strings
```

| Field | Type | Env-aware | Purpose |
|---|---|---|---|
| `auth.require_login` | bool | yes | When true, the proxy gates the service behind Dibbla auth |
| `auth.access_policy` | string | yes | One of `all_members`, `invite_only`, `email_domain` (case-sensitive); empty falls back to deploy-level |
| `auth.google_scopes` | list-of-strings | no | Additional Google OAuth scopes the proxy ensures the user has consented to |

**Env-aware admin UI pattern** — open in dev, locked down in prod, one manifest:

```yaml
services:
  pgadmin:
    image: dpage/pgadmin4:latest
    port: 80
    public:
      default: false
      dev: true
      prod: true
    auth:
      require_login:
        default: true
        dev: false                     # open access for local dev exploration
      access_policy:
        default: invite_only
```

**Fallback semantics.** A public service without an `auth:` block falls back to the deploy-level flags — so existing single-public deploys keep working byte-identically. An empty `auth: {}` block is equivalent to no `auth:` block at all (still falls back). A `require_login: true` without an explicit `access_policy` defaults to `all_members`. A `require_login: false` explicitly clears the deploy-level policy, ensuring the proxy lets traffic through unauthenticated for that service.

**Precedence rule.** `require_login` is the master gate. `require_login: false` (after env-aware resolution) **overrides any `access_policy` value, including one set in the same `auth:` block**. This is what makes the canonical "open in dev, locked in prod" pattern work with a default-everywhere `access_policy`:

```yaml
auth:
  require_login: { default: true, dev: false }    # false in dev, true everywhere else
  access_policy: { default: invite_only }         # applies in every env including dev
```

In dev, env-aware resolution yields `require_login=false` AND `access_policy=invite_only`. The master-gate rule clears the policy → service is open. In prod, `require_login=true` AND `access_policy=invite_only` → service is gated by `invite_only`. One manifest, two behaviors.

**Validation.** Unknown `access_policy` values are rejected at parse time (`MANIFEST_INVALID`). Use `dibbla manifest validate` to catch typos in CI.

---

## 14. Custom domains

A service can claim a custom hostname via `domain:`:

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
    domain: api.example.com
```

The Ingress is rendered with that hostname and the platform's TLS issuer takes care of cert provisioning. **DNS is your job**:

- Create a `CNAME` from `api.example.com` → the platform's ingress hostname (the platform operator publishes the target — usually `<region>.ingress.dibbla.com`).
- Once DNS is live, the cert issuer issues a Let's Encrypt cert (HTTP-01 by default; DNS-01 if the operator has configured a DNS provider).
- `https://<alias>.dibbla.com` is preserved in addition to the custom domain.

Multiple custom domains for the same service aren't supported in v1 (one `domain:` per service). For wildcard or apex domains, talk to the platform operator about the DNS-01 path — apex `example.com` needs a DNS-01 challenge because most registrars don't support `ALIAS`/`ANAME` at the apex.

---

## 15. Cron jobs (`jobs:`)

Top-level `jobs:` declares scheduled jobs. They render to K8s CronJob objects.

```yaml
jobs:
  nightly-cleanup:
    schedule: "0 2 * * *"             # standard cron, UTC
    image: alpine:3.20
    command: [sh, -c, "/usr/local/bin/cleanup.sh"]
    environment:
      RETENTION_DAYS: "30"
    cpu: 200m
    memory: 256Mi
  hourly-warm-cache:
    schedule: "0 * * * *"
    build: ./warmer                   # build a job image from the archive
    successful_jobs_history_limit: 1  # default 3
    failed_jobs_history_limit: 3      # default 1
```

Rules:

- `schedule` is a standard cron expression in UTC; quote it to keep YAML happy with the `*`s.
- One of `image` or `build` (same rules as services).
- `command:` overrides the image's `CMD`.
- `environment:` is flat (no per-env form).
- Job names follow service-name regex (`^[a-z][a-z0-9-]{0,29}$`) and share namespace with services — a job and a service can't have the same name.
- History limits default to K8s defaults (`3` successful, `1` failed). Override per-job for noisy crons.
- A cron-only deploy (no `services:`) is allowed if `--no-public` is passed; otherwise the validator rejects it (`PUBLIC_SERVICE_MISSING`).

---

## 16. Build-time secrets

Some `docker build`s need a secret — a private npm token, a Codeartifact creds file, a registry pull token. These must NOT land in the image layer (anyone with the image can extract them).

```yaml
services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile
      secrets:
        - id: npm_token
          source: NPM_TOKEN_SECRET    # name of the secret in dibbla secrets
```

In your Dockerfile, use BuildKit's `--mount=type=secret`:

```dockerfile
RUN --mount=type=secret,id=npm_token \
    NPM_TOKEN=$(cat /run/secrets/npm_token) npm ci
```

The platform mounts the secret value into the BuildKit Solve via the named id; the value never lands in the image layer. You provide the value via `dibbla secrets set NPM_TOKEN_SECRET <value>` (deployment-wide, since builds happen before per-service routing).

- `id` is the BuildKit identifier referenced in the Dockerfile (`--mount=…,id=<id>`).
- `source` is the name of the secret in the dibbla secrets store. Per-service build secrets are not supported in v1 — the build is one operation per service, and the secret is scoped to that build.
- The secret must exist before the deploy; otherwise `BUILD_FAILED` with a message naming the missing secret.

---

## 17. Shell variable substitution

`dibbla.yaml` accepts docker-compose-style `${VAR}` placeholders that are resolved from your **shell environment** at the moment `dibbla deploy` runs. Useful for `${IMAGE_VERSION}`, `${HOME}`, CI-injected values, etc.

```yaml
version: 1
services:
  web:
    image: ghcr.io/example/web:${BUILD_VERSION:-dev}
    port: 3000
    public: true
    environment:
      APP_VERSION: ${BUILD_VERSION:-dev}
      USER_HOME:   ${HOME}
      REDIS_URL:   ${DIBBLA_SVC_REDIS_URL}
```

**Syntax** (compose-compatible):

| Form | Behavior |
|---|---|
| `${VAR}` | Substitute with `VAR` from the shell env. Fails if `VAR` is unset and has no default. |
| `${VAR:-default}` | Use `default` when `VAR` is unset (`default` may be empty). |
| `$$` | Escape — produces a literal `$`. So `$${VAR}` ships as the literal text `${VAR}` (useful when you want a placeholder in the YAML that the platform DOES NOT substitute). |
| `$VAR` (no braces) | **Not** substituted. Only the brace form is recognized. |

**Reserved prefix: `DIBBLA_*`.** Variables whose name starts with `DIBBLA_` are NEVER substituted by the CLI, even if they happen to be set in your shell. They're reserved for the server's discovery contract (`DIBBLA_SVC_<NAME>_HOST`, `DIBBLA_DEPLOYMENT_ID`, `DIBBLA_ALIAS`, `DIBBLA_ENV`, `DIBBLA_SERVICE_NAME`, etc.) and are filled in at deploy time when the values are actually known. So `${DIBBLA_SVC_REDIS_URL}` in your YAML stays as-is through the CLI and gets substituted server-side; a stray `DIBBLA_ALIAS` in your shell can't shadow the platform value.

**Where it happens.** The CLI substitutes the **root-level `dibbla.yaml`** (or `.yml`) **before uploading the archive**. Files in subdirectories pass through unchanged, so a Dockerfile's `${VAR}` for build args is untouched (BuildKit handles those at build time). Substitution is text-level — comments, blank lines, anchors, and formatting are preserved byte-for-byte except where placeholders are replaced.

**Failure modes.** If `${VAR}` references a variable that's unset and has no default and isn't `DIBBLA_*`, the CLI exits before upload with a clear error naming the variable. So you catch typos like `${DAATBASE_URL}` immediately rather than after a successful deploy with an empty env var.

**Worked example** with CI:

```bash
# .github/workflows/deploy.yml
env:
  BUILD_VERSION: ${{ github.sha }}
  SENTRY_DSN: ${{ secrets.SENTRY_DSN }}

steps:
  - run: dibbla deploy . --alias myapp --target-env prod -m "deploy ${{ github.sha }}"
```

```yaml
# dibbla.yaml — referenced from CI
services:
  web:
    build: ./web
    port: 3000
    public: true
    environment:
      APP_VERSION: ${BUILD_VERSION}    # GitHub SHA
      SENTRY_DSN:  ${SENTRY_DSN:-}     # empty default if unset
      LOG_LEVEL:   info
```

**Difference from server-side `${DIBBLA_*}`:** two non-overlapping substitution layers. The CLI handles user shell vars (anything NOT starting with `DIBBLA_`); the server handles platform discovery vars (`DIBBLA_*`) at render time. Both pass through unchanged on the other side.

---

## 18. Schema validation

Two layers, two purposes:

| Tool | What it checks | When to use |
|---|---|---|
| `dibbla manifest validate` | Local parse + schema (version, service names, build/image XOR, image-must-have-tag, port range) | CI, pre-commit hooks, editor integrations — fast, no network |
| `dibbla preview` | Everything `manifest validate` does + env-aware resolution + profile filter + quota check + (some) cross-service references | Before a real deploy when you want a server-authoritative answer |

Error codes (subset — full set in `reference.md`):

| Code | Meaning |
|---|---|
| `MANIFEST_AMBIGUOUS` | Both `dibbla.yaml` and `dibbla.yml` exist; remove one |
| `MANIFEST_INVALID` | Schema violation (parse error, version, build+image both set, image without tag, …) |
| `MANIFEST_UNSUPPORTED` | Reserved key the schema knows about but doesn't accept yet |
| `SERVICE_NAME_INVALID` | Service name fails regex or is reserved |
| `BUILD_CONTEXT_MISSING` | `build.context` doesn't exist in the archive |
| `DOCKERFILE_MISSING` | `build.dockerfile` doesn't exist at the resolved path |
| `PUBLIC_SERVICE_MISSING` | No `public: true` and `--no-public` not set |
| `PUBLIC_MISSING_PORT` | A `public: true` service has no `port:` |
| `QUOTA_EXCEEDED` | Resolved set exceeds an org quota (services, replicas, CPU, memory, PVC size) |
| `BUILD_FAILED` | A build step failed (missing build secret, Dockerfile error, …) |
| `DEPLOY_IN_PROGRESS` | Another deploy is in-flight for this alias; wait or cancel |
| `PATCH_AMBIGUOUS` | `dibbla apps update --replicas N` against a multi-service deploy |
| `ALIAS_HOSTNAME_COLLISION` | A multi-public deploy would produce a hostname `<alias>-<service>.<base>` that another existing alias in your org already owns. Rename either deploy. |
| `DEPENDS_ON_UNKNOWN` | `depends_on:` references a service name that doesn't exist in the manifest |
| `VOLUME_UNSUPPORTED` | Top-level `volumes:` block is reserved for future versions; use per-service `volumes:` instead |
| `IMAGE_REGISTRY_DENIED` | `image:` references a registry not on the platform's allowlist |
| `INVALID_HEALTHCHECK` / `MISSING_HEALTHCHECK` | Healthcheck declaration violates the schema (e.g. multiple of http_get/tcp_socket/exec, missing required fields) |
| `HEALTHCHECK_FAILED` / `HEALTHCHECK_TIMEOUT` | Probe didn't pass at deploy time — pod won't go ready |
| `SERVICE_NAME_TOO_LONG` | Computed K8s name `{alias}-{service}` exceeds 63 chars; shorten one |
| `ALIAS_EXISTS` | Alias is already in use; pass `--update` or `--force` (single-public deploys) |
| `RESERVED_ALIAS` | The chosen alias matches a platform-reserved name |

Local validation cannot detect everything: env-aware errors (e.g. `replicas: { prod: -1 }`) and quota violations are server-side. A manifest that passes `dibbla manifest validate` may still fail `dibbla preview`.

---

## 19. Quotas

Default org quotas (the platform operator can override):

| Quota | Default | Field |
|---|---|---|
| Max services per deploy | 8 | service count after profile filter |
| Max replicas per service | 10 | per-service `replicas` |
| Max replicas total | 20 | sum of `replicas` across services |
| Max CPU total | 8 | sum of `cpu` across services (cores) |
| Max memory total | 16Gi | sum of `memory` across services |
| Max PVC size per service | 10Gi | sum of `volumes[].size` per service |
| Max PVC size total | 50Gi | sum across services |
| Max manifest size | 64KB | the `dibbla.yaml` file itself |
| Max archive size | 50MB | the deploy upload (CLI cap) |

`QUOTA_EXCEEDED` errors carry a `path` like `services.worker.replicas` and a `detail` like `replicas 12 exceeds per-service max 10`. Override at the org level by talking to the platform operator.

---

## 20. Reserved keys

The schema accepts these top-level keys at parse time but rejects them at validate time with `MANIFEST_UNSUPPORTED`. They're held for future versions to avoid users squatting on names that will mean something specific.

- `volumes:` — top-level shared/named PVCs.
- `networks:` — explicit named networks (today every deploy has one default).
- `secrets:` — top-level secret declarations (today secrets live in the secrets store).
- `cron:` — superseded by top-level `jobs:`.
- `init:` — top-level init containers (use per-service `init:` instead).

If you set one of these the validator emits `MANIFEST_UNSUPPORTED at <key>` and the deploy fails. The fix is the per-service equivalent.

---

## 21. Worked example: web + worker + redis

Annotated, end-to-end. This is the canonical multi-service shape — copy and adapt.

```yaml
version: 1

services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile
      secrets:
        - id: npm_token
          source: NPM_TOKEN_SECRET
    port: 3000
    public: true
    domain: api.example.com           # optional custom domain
    replicas:
      default: 1
      prod: 3
    cpu:
      default: 250m
      prod: 1
    memory:
      default: 256Mi
      prod: 1Gi
    environment:
      default:
        LOG_LEVEL: info
        NODE_ENV: production
      prod:
        SENTRY_DSN: ${SENTRY_DSN}     # comes from a runtime secret of the same name
      staging:
        LOG_LEVEL: debug
    init:
      - name: migrate
        image: registry.example.com/migrate:v1
        command: [migrate, up]
        environment:
          DATABASE_URL: ${DATABASE_URL}
    healthcheck:
      liveness:
        http_get: { path: /healthz }
        period_seconds: 10
        failure_threshold: 3
      readiness:
        http_get: { path: /ready }
        period_seconds: 5
    depends_on: [redis]
    expose_to: []                     # public; pod-to-pod default open

  worker:
    build: ./worker
    replicas:
      default: 1
      prod: 2
    environment:
      LOG_LEVEL: info
      REDIS_URL: ${DIBBLA_SVC_REDIS_URL}
    depends_on: [redis]
    expose_to: [web]                  # only the web service can reach worker

  redis:
    image: redis:7
    port: 6379
    volumes:
      - path: /data
        size: 2Gi
    expose_to: [web, worker]

  mailcatcher:
    image: mailhog/mailhog:v1.0.1
    port: 8025
    profiles: [dev]                   # only deployed when --profile dev is passed

jobs:
  nightly-cleanup:
    schedule: "0 2 * * *"
    image: alpine:3.20
    command: [sh, -c, "/cleanup.sh"]
    environment:
      RETENTION_DAYS: "30"
```

Validate it locally:

```bash
dibbla manifest validate              # ./dibbla.yaml
```

Preview against staging:

```bash
dibbla preview --target-env staging
```

Preview against prod with mailcatcher activated:

```bash
dibbla preview --target-env prod --profile dev
```

Deploy to prod (no profiles → `mailcatcher` is skipped):

```bash
dibbla deploy --alias myapp --target-env prod -m "feat: ship multi-service"
```

Operate per-service afterwards:

```bash
dibbla logs myapp --service worker -f
dibbla apps restart myapp --service worker
dibbla secrets set NPM_TOKEN_SECRET <token> -d myapp           # build-time secret
dibbla secrets set SENTRY_DSN https://... -d myapp --service web
```

---

## Cross-references

- [reference.md](reference.md) — exhaustive flag tables for `dibbla manifest validate`, `dibbla preview`, the new `--target-env` / `--profile` / `--no-public` flags on `deploy`, and the per-service `--service` flag on `apps restart` / `logs` / `secrets`.
- [examples.md](examples.md) — runnable bash transcripts for each multi-service pattern (init container, healthcheck, custom domain, cron, build secret, etc.).
- [platform.md § 8.5](platform.md) — runtime contract under multi-service: discovery env vars, NetworkPolicy, public URL shape, what does and doesn't work compared to the single-Dockerfile path.
- [guardrails.md](guardrails.md) Check 6 — pre-deploy multi-service safety (quota fit, no `depends_on:` cycles, init exit, healthcheck thresholds, build-secret existence, multi-public confirmation).
