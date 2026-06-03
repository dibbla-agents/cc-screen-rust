# Dibbla CLI — Examples

Copy-paste examples for common workflows. For full usage and flags see [reference.md](reference.md).

---

## Installing dibbla

```bash
# macOS — Homebrew
brew install dibbla-agents/tap/dibbla

# macOS / Linux — shell installer (drops binary into ~/.local/bin)
curl -fsSL https://install.dibbla.com/install.sh | sh

# Windows — PowerShell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://install.dibbla.com/install.ps1 | iex"

# Verify
dibbla --version

# Upgrade (same command as install — replaces in place)
curl -fsSL https://install.dibbla.com/install.sh | sh
# …or on macOS Homebrew:
brew upgrade dibbla
```

---

## Login (including Claude Code / agentic tools)

```bash
# Interactive (real TTY — picks between browser OAuth and paste-token)
dibbla login api.dibbla.net

# From inside Claude Code's `!` prefix, an agent shell, or any non-TTY context
! dibbla login --browser api.dibbla.net
# Opens your default browser via localhost callback. No stdin needed.

# Headless (no browser available — CI, SSH, containers)
dibbla login api.dibbla.net --api-key ak_...
# or:
export DIBBLA_API_TOKEN=ak_...
export DIBBLA_API_URL=https://api.dibbla.net
dibbla deploy .          # reads env vars directly; no login needed

# Cloud VM / SSH / Docker (no keyring service installed) — CLI >= v1.2.21
# `dibbla login` auto-detects a missing keyring and falls back to a user-level
# credentials file at ~/.config/dibbla/credentials.env (mode 0600). Behaves
# like the keyring: machine-wide, persists across `cd`. No flags needed.
dibbla login --api-key=ak_... --api-url=https://api.dibbla.net

# Add --write-env if you also want credentials materialized in ./.env (e.g.
# for `docker compose` to pick them up):
dibbla login --api-key=ak_... --write-env

# CLI < v1.2.21 (no auto-fallback): use --no-keychain --write-env so the CLI
# skips the failing keyring write and lands creds in ./.env instead.
dibbla login --api-key=ak_... --api-url=https://api.dibbla.net --write-env --no-keychain

# Afterwards, every dibbla command from any directory reads credentials:
dibbla deploy .
dibbla apps list

# Log out (clears keyring AND user-level credentials file)
dibbla logout
```

---

## Status (which API server, am I logged in, what version?)

```bash
# Human-readable: version + API URL + token + live validation
dibbla status

# Skip the network validation call (offline / fast diagnostic)
dibbla status --no-validate

# Machine-readable for scripts and CI gates
dibbla status --json | jq '.logged_in'
dibbla status --json | jq -r '.api_url'

# Use as a precondition before commands that require login
if ! dibbla status --json | jq -e '.logged_in' >/dev/null; then
  echo "Not logged in" >&2; exit 1
fi
```

Output annotates each value with the source it was read from (`env (DIBBLA_API_URL)`, `env (DIBBLA_API_TOKEN)`, `keyring`, `credentials file`, `default`, `none`). Useful for diagnosing "this shell deploys somewhere different from that one" — it's almost always a stray `DIBBLA_API_URL` in one shell's environment shadowing the keyring value.

Exit codes: `0` when logged in (or `--no-validate` and a token is configured), `3` when not logged in or the token was rejected.

---

## Running task files locally

```bash
# Run ./dibbla-task.yaml in the current directory
dibbla run

# Run a specific local task file
dibbla run ./setup/dibbla-task.yaml

# Preview (parse + print plan, do not execute)
dibbla run --preview ./dibbla-task.yaml

# Run a bootstrap yaml from GitHub — clones into your CWD and runs setup
mkdir my-project && cd my-project
dibbla run https://raw.githubusercontent.com/dibbla-agents/dibbla-public-templates/master/getting-started.dibbla-task.yaml

# Override env vars and working directory
dibbla run --env PORT=3000 --env-file .env.local --work-dir ./build ./dibbla-task.yaml

# Switch output format for CI / GitHub Actions
dibbla run --format gh ./dibbla-task.yaml
```

---

## Discovering and installing templates

```bash
# See what's available
dibbla template list

# Force re-fetch + show manifest source (cache / network / embedded)
dibbla template list --refresh -v

# Install a template into its default-named directory
dibbla template install expense-reporter
# → creates ./expense-reporter-template-1 and runs the bootstrap pipeline

# Install a template into a custom directory
dibbla template install getting-started my-starter-app

# Reuse an existing destination directory
dibbla template install crm --force
```

---

## Skills (teach AI coding agents about the CLI)

Install the bundled `dibbla` skill so every coding agent in the project reads it automatically. The skill content is embedded in the CLI binary — no network needed, and the skill version is locked to your installed `dibbla` version.

```bash
# see what skills are bundled (one for now: 'dibbla')
dibbla skills list

# install into the current project
dibbla skills install dibbla
# → writes .claude/skills/dibbla/{SKILL.md,examples.md,guardrails.md,reference.md}
#   and an AGENTS.md + GEMINI.md pointer block at the project root

# install into $HOME once so every project picks up the skill
dibbla skills install dibbla --user

# Claude Code only (skip AGENTS.md and GEMINI.md)
dibbla skills install dibbla --no-agents

# re-run is a clean no-op if nothing changed;
# use --force to restore skill files that were edited locally
dibbla skills install dibbla --force
```

**What each output does:**

| File | Used by |
|------|---------|
| `.claude/skills/dibbla/SKILL.md` | Claude Code (native skill format, gives `/dibbla` slash command) |
| `AGENTS.md` | Cursor, Opencode, Codex, Copilot, Windsurf, Aider, Zed, Warp, RooCode (AGENTS.md open standard) |
| `GEMINI.md` | Gemini CLI (its default context filename) |

`AGENTS.md` and `GEMINI.md` use a marker-delimited block (`<!-- >>> dibbla skill >>> -->` … `<!-- <<< dibbla skill <<< -->`). Running `dibbla skills install dibbla` again replaces only that block — any other content you've added to those files is preserved byte-for-byte.

**Inside a `dibbla-task.yaml` bootstrap step:**

```yaml
- id: install-skills
  name: "Install Dibbla Skill"
  type: command
  run: "dibbla skills install dibbla"
  depends_on: ["update-dibbla"]
```

The `depends_on: ["update-dibbla"]` ensures the CLI is fresh enough to have the `skills` command before this step runs.

---

## Feedback

```bash
dibbla feedback "The deploy took too long"
dibbla feedback Everything is on fire
dibbla feedback "Why does the database keep disappearing?"

# List all feedback
dibbla feedback list

# Delete feedback
dibbla feedback delete 550e8400-e29b-41d4-a716-446655440000
dibbla feedback delete 550e8400-e29b-41d4-a716-446655440000 --yes
```

---

## Deploy

```bash
dibbla deploy
dibbla deploy ./my-app
dibbla deploy --alias my-api       # Custom alias instead of directory name
dibbla deploy --force
dibbla deploy --update             # Rolling update (zero downtime)
dibbla deploy -e NODE_ENV=production -e LOG_LEVEL=info
dibbla deploy --cpu 500m --memory 512Mi --port 3000
dibbla deploy --favicon https://example.com/favicon.ico
dibbla deploy ./ --cpu 500m --memory 512Mi -e NODE_ENV=production

# Deploy with login guard
dibbla deploy --alias my-app --require-login
dibbla deploy --alias my-app --require-login --access-policy invite_only
dibbla deploy --alias my-app --require-login --google-scopes https://www.googleapis.com/auth/drive.readonly
```

### Deploy troubleshooting

#### Cloudflare 524 / "timeout occurred" during deploy

`dibbla deploy` holds a single HTTP connection to the backend while the container image is built. Builds that take longer than ~100 seconds (common for Next.js, Rails, or large monorepos) may return a Cloudflare 524 on the client side **even though the backend build is still running and often succeeds.** A 524 is not necessarily a failure.

Recovery:

1. Wait 2–5 minutes for the backend build to finish.
2. Run `dibbla apps list` and look for the alias.
3. If it appears with `running` status, the deploy succeeded — you are done.
4. If the alias does not appear after ~10 minutes, retry with `dibbla deploy --update` (rolling, zero downtime if the previous attempt did quietly succeed). Avoid `--force`, which causes downtime if the deploy actually worked.

---

## Multi-service deployments (`dibbla.yaml`)

For the schema and runtime contract see [manifest.md](manifest.md). The transcripts below are the day-to-day shapes.

### A minimal multi-service app

```yaml
# dibbla.yaml
version: 1
services:
  web:
    build: ./web
    port: 3000
    public: true
    environment:
      REDIS_URL: ${DIBBLA_SVC_REDIS_URL}     # service discovery
  worker:
    build: ./worker
  redis:
    image: redis:7
    port: 6379
```

```bash
dibbla deploy --alias myapp -m "feat: ship multi-service"
```

The deploy-api builds web + worker in parallel, pulls redis, applies the K8s graph atomically, and rolls back on any failure. The success line names every active service.

### Local iteration with docker-compose

The Dibbla platform doesn't run `dibbla.yaml` locally — `dibbla preview` is a server-side dry run, not a local stack. For tight inner-loop dev (edit code → see it live in seconds), mirror the manifest into a `docker-compose.yml` next to it.

```yaml
# dibbla.yaml — what runs on Dibbla
version: 1
services:
  web:
    build: ./web
    port: 80
    public: true
    environment:
      MONGO_URL: mongodb://${DIBBLA_SVC_MONGO_HOST}:${DIBBLA_SVC_MONGO_PORT}/
    depends_on: [mongo]
  mongo:
    image: mongo:7
    port: 27017
    expose_to: [web]
    volumes:
      - { path: /data/db, size: 1Gi }
```

```yaml
# docker-compose.yml — what runs on your laptop
services:
  web:
    build: ./web
    ports:
      - "8080:80"                                  # bind to host port (avoid clashes)
    environment:
      MONGO_URL: mongodb://mongo:27017/            # compose DNS uses the service name
    depends_on:
      - mongo
  mongo:
    image: mongo:7
    volumes:
      - mongo-data:/data/db
volumes:
  mongo-data:
```

Mapping cheat-sheet — the two formats line up almost field-for-field:

| `dibbla.yaml` | `docker-compose.yml` | Notes |
|---|---|---|
| `services.<name>` | `services.<name>` | Same |
| `build: ./dir` | `build: ./dir` | Same |
| `image: foo:tag` | `image: foo:tag` | Same |
| `port: 80` | `ports: ["8080:80"]` | Compose binds container port to a host port; pick an unused one |
| `environment:` | `environment:` | Same shape (compose has no env-aware maps) |
| `${DIBBLA_SVC_MONGO_HOST}` | `mongo` (service name) | Compose uses Docker DNS — no substitution layer |
| `${DIBBLA_SVC_MONGO_URL}` | `http://mongo:<port>` | Spell out protocol + port locally |
| `volumes: [{path: /data/db, size: 1Gi}]` | named volume + `mongo-data:/data/db` | Compose has no size enforcement |
| `expose_to: [...]` | (omit) | No NetworkPolicy equivalent; default open |
| `profiles: [dev]` | (omit, or compose's own `profiles:`) | Run all locally, or use compose profiles for parity |
| `public: true` | (omit) | The `ports:` host binding is your local "public" |
| `auth:` / `domain:` | (omit) | Platform-only |
| `init:` containers | compose has no native init | Use `depends_on` + a healthcheck, or run the init manually |
| Cron `jobs:` | (move to a shell script or compose `command`) | Compose has no native CronJob |
| Build-time secrets (`build.secrets`) | shell env / `.env` next to compose | Compose passes `.env` automatically |

Run it:

```bash
docker compose up --build -d
docker compose logs -f web
docker compose down       # stop, keep volumes
docker compose down -v    # stop and wipe volumes
```

When the app behaves locally, `dibbla preview --target-env <env>` is the right next step (server-authoritative env-aware/profile/quota check), then deploy:

```bash
dibbla deploy . --alias myapp -m "feat: ..."
```

**Caveats — the two stacks are similar but not identical:**

- No `${DIBBLA_SVC_*}` substitution locally — compose uses plain Docker DNS.
- No NetworkPolicy locally — `expose_to:` is silently relaxed.
- No env-aware resolution or profile filtering locally — you run whatever is in the compose file.
- No build-time secrets injected from `dibbla secrets` — set them via shell env or `.env` instead.
- No TLS, public URLs, custom `domain:`, or `auth:` gating — those are platform-only.

So compose is the inner loop; `dibbla preview` is the right outer-loop check before the actual deploy.

### Validate a manifest locally (no network)

```bash
dibbla manifest validate                       # ./dibbla.yaml
dibbla manifest validate ./apps/myapp          # validate ./apps/myapp/dibbla.yaml
dibbla manifest validate --json | jq '.services[] | select(.public)'
```

CI use:
```bash
dibbla manifest validate --json > validate.json && jq -e '.valid' validate.json
```

Local check covers schema only: parse, version, service-name regex, build/image XOR, image-must-have-tag, port range. Env-aware resolution and quota run server-side — for those, use `dibbla preview`.

### Server-authoritative preview before a real deploy

```bash
dibbla preview --target-env prod
dibbla preview --target-env staging --profile mailcatcher
dibbla preview --no-public                    # cron-only or worker-only is OK
dibbla preview --json | jq '.active_services[] | {name, replicas}'
```

The server resolves env-aware fields, applies profiles, runs quota, and reports the final shape — no build, no apply, no deploy slot used.

### Target a specific environment block

```yaml
# dibbla.yaml fragment
services:
  web:
    build: ./web
    port: 3000
    public: true
    replicas:
      default: 1
      staging: 2
      prod: 5
    environment:
      default: { LOG_LEVEL: info }
      prod:    { LOG_LEVEL: warn, SENTRY_DSN: ${SENTRY_DSN} }
```

```bash
dibbla deploy --alias myapp --target-env staging -m "deploy: staging"
dibbla deploy --alias myapp --target-env prod    -m "release: v2.4"
```

Env-aware fields resolve in this order: explicit env-key → `default:` → field's documented default. The CLI's `--target-env prod` is the same string the manifest resolver uses.

### Activate manifest profiles

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
  mailcatcher:
    image: mailhog/mailhog:v1.0.1
    port: 8025
    profiles: [dev]                           # only deployed when profile is active
  metrics-shipper:
    image: prom/prometheus:v2.50
    port: 9090
    profiles: [observability]
```

```bash
dibbla deploy --alias myapp --profile dev --profile observability -m "feat: ship"
```

Profiles are additive. Skipped services appear in the deploy event stream and in `dibbla preview` output.

### Worker-only deploy (`--no-public`)

```yaml
services:
  worker:
    build: .                                  # no public service
```

```bash
dibbla deploy --alias background-jobs --no-public -m "feat: nightly job runner"
```

Without `--no-public` the validator emits `PUBLIC_SERVICE_MISSING`. Cron-only deploys (top-level `jobs:` only, no `services:`) also need `--no-public`.

### Cron-only deploy (top-level `jobs:`)

```yaml
version: 1
jobs:
  daily-report:
    schedule: "0 9 * * *"
    image: alpine:3.20
    command: [sh, -c, "/run-report.sh"]
    environment:
      SLACK_WEBHOOK: ${SLACK_WEBHOOK}
```

```bash
dibbla secrets set SLACK_WEBHOOK https://hooks.slack.com/... -d daily
dibbla deploy --alias daily --no-public -m "feat: daily report job"
```

### Inspect per-service status

```bash
dibbla apps list                              # alias, URL, status, last deployed
dibbla apps update myapp --replicas 3         # rejected with PATCH_AMBIGUOUS on multi-service
                                              # (edit dibbla.yaml + redeploy --update instead)
```

### Restart a single service (rolling)

```bash
dibbla apps restart myapp --service worker
dibbla apps restart myapp -s web --quiet
dibbla apps restart myapp -s redis --json | jq '.status'
```

Idempotent — calling twice in a row produces two pod rollouts.

### Tail logs for the whole deployment (all services merged)

```bash
dibbla logs myapp                       # last 15 min, every service in the deployment
dibbla logs myapp -f --since 10m        # backfill 10 min then follow, all services
dibbla logs myapp --grep "ERROR" --since 1h
dibbla logs myapp --json | jq '{svc: .labels.service, line}'
```

Omit `--service` and Loki returns lines from every container in the deployment, interleaved by timestamp. Each NDJSON entry carries a `labels.service` field so you can attribute lines to the originating service when reading `--json`. This is the default and the right starting point for "what is my deployment doing right now" or "where's the error coming from across services".

### Follow one service's logs

```bash
dibbla logs myapp --service web -f --since 5m
dibbla logs myapp --service worker --grep "ERROR"
dibbla logs myapp --service redis --json | jq '.line'
```

Server forwards `?service=worker` to the existing Loki backend (cross-service, retained, supports `--grep`). Use this once the aggregated view points you at a specific service.

### Stream pod logs without Loki

```bash
dibbla logs myapp --service web --pod-stream -f --tail 100
```

When Loki isn't configured (or you specifically want the K8s-direct stream), `--pod-stream` switches to the K8s API endpoint. Each line is prefixed with `[<pod>] ` — useful for tracing which replica produced an error.

### Scope a secret to one service

```bash
# Per-service secret: only the web container sees this
dibbla secrets set NPM_TOKEN xxx -d myapp --service web

# Deployment-wide: every service sees this
dibbla secrets set DATABASE_URL postgres://... -d myapp

# Org-global: every deployment sees this
dibbla secrets set SHARED_API_KEY abc

# List by scope
dibbla secrets list -d myapp                  # deployment-wide entries (service_name='')
dibbla secrets list -d myapp --service web    # per-web entries only
dibbla secrets list                           # global only
```

Precedence inside the web container at runtime: per-web (`NPM_TOKEN`) > deployment-wide (`DATABASE_URL`) > global (`SHARED_API_KEY`).

### Init container for migrations

```yaml
services:
  api:
    build: ./api
    port: 8080
    public: true
    init:
      - name: migrate
        image: registry.example.com/migrate:v1
        command: [migrate, up]
        environment:
          DATABASE_URL: ${DATABASE_URL}       # from a deployment-wide secret
```

```bash
dibbla secrets set DATABASE_URL postgres://... -d api
dibbla deploy --alias api -m "feat: add migrate-on-deploy"
```

The `migrate` init runs to completion before the main `api` container starts on every pod.

### Healthchecks (liveness / readiness / startup)

```yaml
services:
  api:
    build: ./api
    port: 8080
    public: true
    healthcheck:
      liveness:
        http_get: { path: /healthz, port: 8080 }
        period_seconds: 10
        failure_threshold: 3
      readiness:
        http_get: { path: /ready }
        period_seconds: 5
      startup:
        tcp_socket: { port: 8080 }
        failure_threshold: 30
```

`startup` lets a slow-booting container have 30 × 10s = 5 minutes before liveness kicks in — protects JVM/Rails-style apps from being killed at second 10.

### Multiple public services (hyphenated host)

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true                              # https://myapp.dibbla.com (lex-first → bare alias)
  api:
    build: ./api
    port: 8080
    public: true                              # https://myapp-api.dibbla.com
```

URL shape:

- The lex-first public service ("primary") owns the bare `<alias>.<base-domain>` for backwards compatibility — here `api` (alphabetical), so `https://myapp.dibbla.com` serves the API. Edit: with `web` lex-first, the bare alias would serve `web` instead.
- Other public services get `<alias>-<service>.<base-domain>` — one DNS label deep so the existing `*.dibbla.com` wildcard cert covers them without per-deploy wildcard issuance.
- Want a specific service at the bare alias regardless of lex order? Give it a `domain:` field with a custom hostname, or just rename the service so it's first alphabetically.

If your alias plus a service name would collide with another existing alias (e.g. you deploy `myapp` with a `web` service while `myapp-web` already exists), the deploy fails with `ALIAS_HOSTNAME_COLLISION` before any side effects.

### Per-service auth — open web + locked-down pgadmin

The canonical pattern for the dev-vs-prod admin UI question. Web is open to the world; pgadmin is reachable but only for invited users. Env-aware fields make the manifest one file across environments.

```yaml
version: 1
services:
  web:
    build: ./web
    port: 3000
    public: true                              # always open
  pgadmin:
    image: dpage/pgadmin4:latest
    port: 80
    public:
      default: false                          # not deployed in unspecified envs
      dev: true
      prod: true
    auth:
      require_login:
        dev: false                            # anyone in your Dibbla org can reach it in dev
        prod: true                            # locked down in prod
      access_policy:
        prod: invite_only                     # only specifically-invited users in prod
```

Resulting URLs after deploy:

```bash
dibbla deploy . --alias myapp --target-env dev -m "deploy dev"
# https://myapp.dibbla.com              (web — open)
# https://myapp-pgadmin.dibbla.com      (pgadmin — open within Dibbla)

dibbla deploy . --alias myapp --target-env prod -m "deploy prod"
# https://myapp.dibbla.com              (web — open)
# https://myapp-pgadmin.dibbla.com      (pgadmin — login + invite_only)
```

A public service without an `auth:` block falls back to the deploy-level `--require-login` / `--access-policy` flags, so existing single-public deploys keep working without changes.

**Precedence rule:** `require_login` is the master gate. `require_login: false` overrides any `access_policy` value — including one set in the same block. So you can write the equivalent variant with `access_policy: { default: invite_only }` instead of `prod: invite_only` and the dev override still works:

```yaml
auth:
  require_login: { default: true, dev: false }    # false in dev, true elsewhere
  access_policy: { default: invite_only }         # applies in every env
# In dev: require_login=false → policy is cleared, service open.
# In prod: require_login=true + invite_only → service gated.
```

### Custom domain

```yaml
services:
  web:
    build: ./web
    port: 3000
    public: true
    domain: api.example.com
```

DNS is your job: point `CNAME api.example.com → <region>.ingress.dibbla.com` (the platform operator publishes the target). Once DNS is live, the platform issues a TLS cert via Let's Encrypt automatically. `https://<alias>.dibbla.com` keeps working in addition to the custom domain.

### Build-time secret

```yaml
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
```

```dockerfile
# web/Dockerfile fragment
RUN --mount=type=secret,id=npm_token \
    NPM_TOKEN=$(cat /run/secrets/npm_token) npm ci
```

```bash
dibbla secrets set NPM_TOKEN_SECRET <token> -d myapp
dibbla deploy --alias myapp -m "feat: private dep added"
```

The secret value is mounted into the BuildKit Solve via the named id and never lands in the image layer. After `dibbla deploy`, `kubectl exec deploy/myapp-web -- ls /run/secrets/` will be empty in the running container — the secret only existed during the build step that referenced it.

### Shell variable substitution (`${VAR}` in dibbla.yaml)

Compose-style shell-env substitution lets you parametrize the manifest from CI or your local shell without committing values to the file:

```yaml
# dibbla.yaml
services:
  web:
    image: ghcr.io/example/web:${BUILD_VERSION:-dev}
    port: 3000
    public: true
    environment:
      APP_VERSION: ${BUILD_VERSION:-dev}
      SENTRY_DSN:  ${SENTRY_DSN:-}
      USER_HOME:   ${HOME}
      REDIS_URL:   ${DIBBLA_SVC_REDIS_URL}
```

```bash
BUILD_VERSION=v1.2.3 SENTRY_DSN=https://x@sentry.io/123 dibbla deploy . --alias myapp -m "release v1.2.3"
```

Rules:

- `${VAR}` is substituted from the shell env at the moment `dibbla deploy` runs.
- `${VAR:-default}` provides a fallback when the var is unset.
- `${VAR}` with no shell value AND no default — the CLI errors before upload, naming the variable. Catches typos like `${DAATBASE_URL}`.
- Variables starting with `DIBBLA_` are **reserved** — they pass through to the server unchanged, regardless of your shell. Lets `${DIBBLA_SVC_REDIS_URL}` and friends work as documented (server fills them in at render time).
- Use `$$` to escape — `$${LITERAL}` ships as the literal text `${LITERAL}` in the YAML the server sees.

CI integration with GitHub Actions:

```yaml
# .github/workflows/deploy.yml
env:
  BUILD_VERSION: ${{ github.sha }}
  SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
steps:
  - uses: actions/checkout@v4
  - run: dibbla deploy . --alias myapp --target-env prod -m "deploy ${{ github.sha }}"
```

The secret value is mounted into the BuildKit Solve via the named id and never lands in the image layer.

### Re-running a failed deploy

```bash
dibbla deploy --alias myapp --update -m "fix: handle null org in /api/me"   # rolling, zero downtime
dibbla deploy --alias myapp --force  -m "redeploy: nuke and restart"        # tears down + recreates
```

`--update` and `--force` are mutually exclusive. Prefer `--update`. `--force` is appropriate when you've corrupted state and want the deployment recreated from scratch.

### Admin: force an orphan sweep

```bash
DIBBLA_ADMIN_TOKEN=$ADMIN_TOKEN dibbla admin reconcile
DIBBLA_ADMIN_TOKEN=$ADMIN_TOKEN dibbla admin reconcile --json | jq .deployments
```

Reads `DIBBLA_ADMIN_TOKEN` from env (NOT the user's API token). Reaches the deploy-api at `DIBBLA_API_URL`. Prints the K8s objects the reconciler swept (Deployments, Services, Ingresses). The endpoint only exists if the operator configured it; you'll see a 404 otherwise.

---

## Apps

```bash
dibbla apps list
dibbla apps update myapp -e NODE_ENV=production
dibbla apps update myapp -e NODE_ENV=production -e LOG_LEVEL=info
dibbla apps update myapp --replicas 3
dibbla apps update myapp --cpu 500m --memory 512Mi --port 3000
dibbla apps update myapp --replicas 2 --cpu 1 --memory 512Mi -e NODE_ENV=production
dibbla apps update myapp --favicon https://example.com/favicon.ico
dibbla apps update myapp --favicon ""   # Clear favicon

# Login guard settings
dibbla apps update myapp --require-login true
dibbla apps update myapp --require-login false          # Disable login guard
dibbla apps update myapp --access-policy invite_only
dibbla apps update myapp --access-policy ""             # Clear access policy
dibbla apps update myapp --google-scopes https://www.googleapis.com/auth/drive.readonly
dibbla apps update myapp --google-scopes https://www.googleapis.com/auth/drive.readonly --google-scopes https://www.googleapis.com/auth/calendar.readonly
dibbla apps delete my-old-app
dibbla apps delete my-old-app -y
```

---

## Db

```bash
dibbla db list
dibbla db list -q
dibbla db create my-new-db
dibbla db create --name my-new-db
dibbla db create mydb --deployment myapp   # Scope DB + DATABASE_URL secret to a deployment
dibbla db delete my-old-db
dibbla db delete my-old-db --yes
dibbla db delete my-old-db --yes -q
dibbla db dump my-production-db
dibbla db dump my-production-db -o backup.dump
dibbla db restore my-staging-db --file backup.dump
dibbla db restore my-staging-db -f /tmp/backup.dump
dibbla db connect myapp                    # Print connection string with tips
dibbla db connect myapp -q                 # Connection string only (scripting)
psql $(dibbla db connect myapp -q)         # Quick connect
export DATABASE_URL=$(dibbla db connect myapp -q)  # Export as env var
```

---

## Secrets

**Global (omit `-d`):**

```bash
dibbla secrets list
dibbla secrets set API_KEY "my-secret-value"
echo "my-secret-value" | dibbla secrets set API_KEY
dibbla secrets get API_KEY
dibbla secrets delete API_KEY --yes
```

**Per-deployment (`-d` or `--deployment`):**

```bash
dibbla secrets list -d myapp
dibbla secrets set API_KEY "x" -d myapp
dibbla secrets set DATABASE_URL "postgres://..." --deployment myapp
cat private.key | dibbla secrets set SSL_KEY -d myapp
dibbla secrets get API_KEY -d myapp
dibbla secrets delete API_KEY -d myapp -y
```

---

## Workflows

```bash
# List and inspect
dibbla workflows list
dibbla workflows list -o json
dibbla wf list                        # alias

# Get a workflow definition
dibbla workflows get my-workflow
dibbla workflows get my-workflow -o json
dibbla workflows get my-workflow --revision abc123

# Create, update, validate
dibbla workflows create -f workflow.yaml
dibbla workflows update my-workflow -f workflow.yaml
dibbla workflows validate -f workflow.yaml

# Delete
dibbla workflows delete my-workflow
dibbla workflows delete my-workflow --yes

# Execute
dibbla workflows execute my-workflow
dibbla workflows execute my-workflow --data '{"query": "hello"}'
dibbla workflows execute my-workflow -f input.json
dibbla workflows execute my-workflow --node api_node_1

# URL and API docs
dibbla workflows url my-workflow
dibbla workflows api-docs my-workflow
dibbla workflows api-docs my-workflow -o json
```

---

## Nodes

```bash
# Add a node from a file
dibbla nodes add my-workflow -f node.yaml

# Add a node inline
dibbla nodes add my-workflow --inline '{"id":"transform","type":"code","code":"return input"}'

# Remove a node
dibbla nodes remove my-workflow transform
dibbla nodes remove my-workflow transform --yes
```

---

## Edges

```bash
# Add an edge between nodes
dibbla edges add my-workflow "input.output -> transform.input"

# Remove an edge
dibbla edges remove my-workflow "input.output -> transform.input"

# List all edges
dibbla edges list my-workflow
dibbla edges list my-workflow -o json
```

---

## Inputs

```bash
# Set a node input value
dibbla inputs set my-workflow agent1 system_prompt "You are a helpful assistant"
dibbla inputs set my-workflow agent1 temperature 0.7

# Set to null
dibbla inputs set my-workflow agent1 max_tokens ignored --null
```

---

## Tools

```bash
# Add a tool to an agent node
dibbla tools add my-workflow agent1 web_search
dibbla tools add my-workflow agent1 calculator

# Remove a tool
dibbla tools remove my-workflow agent1 web_search
```

---

## Revisions

```bash
# List revisions
dibbla revisions list my-workflow
dibbla rev list my-workflow           # alias
dibbla revisions list my-workflow -o json

# Create a snapshot
dibbla revisions create my-workflow
dibbla revisions create my-workflow -q   # prints only the revision ID

# Restore
dibbla revisions restore my-workflow abc123
```

---

## Functions

```bash
# List available functions
dibbla functions list
dibbla fn list                        # alias
dibbla functions list --server my-server
dibbla functions list --tag search
dibbla functions list -o json

# Get function details
dibbla functions get my-server web_search
dibbla functions get my-server web_search -o json
```

---

## Scripting tips

- Use `-y` / `--yes` to skip confirmations: `apps delete`, `db delete`, `secrets delete`, `workflows delete`, `nodes remove`.
- Use `-q` / `--quiet` on `db list`, `db delete`, `db connect`, and workflow commands for minimal output.
- Use `-o json` on workflow commands for machine-readable output.
- Pipe `secrets get` into env or other commands; use `db list -q` for name-only loops.
- `revisions create -q` prints only the revision ID for scripting.

```bash
# Save a revision and capture the ID
REV=$(dibbla revisions create my-workflow -q)
echo "Created revision: $REV"

# Export a secret
export API_KEY=$(dibbla secrets get API_KEY -d myapp)

# Loop over databases
for db in $(dibbla db list -q); do echo "$db"; done

# Validate before deploying a workflow
dibbla workflows validate -f workflow.yaml && dibbla workflows update my-workflow -f workflow.yaml
```

---

## Agent workflows

Step-by-step patterns for AI agents deploying and managing apps non-interactively.

### Pre-deploy guardrails workflow

```bash
# 1. Code is ready — run guardrails checks by reviewing the source code
#    Check for: security issues, database anti-patterns, unsafe API calls, external write safety
#    See guardrails.md for the full checklist

# 2. Present the guardrails report to the user (example):
#
#    ## Pre-deploy guardrails report
#    - [x] Security (OWASP Top 10): OK
#    - [x] Database usage: OK
#    - [x] REST/API calls: 1 warning
#      - WARNING: No retry logic on payment API call in src/services/payment.js:23
#    - [x] External writes: OK
#    **Result: PASSED with 1 warning**
#
#    Ask: "There is 1 warning. Should I fix this before deploying, or proceed as-is?"

# 3. Wait for user confirmation before deploying or fixing anything

# 4. Deploy only after explicit user approval
dibbla deploy . --alias my-app --update
```

### Deploy a new app (first time)

```bash
# 0. The directory must contain a Dockerfile — dibbla does NOT autodetect
#    languages or run buildpacks. Minimal example:
#
#    FROM node:20-alpine AS build
#    WORKDIR /app
#    COPY package*.json ./
#    RUN npm ci --omit=dev
#    COPY . .
#    EXPOSE 3000
#    CMD ["node", "server.js"]
#
# 1. Check if the app already exists
dibbla apps list

# 2. If NOT listed, deploy with all required env vars in one command
dibbla deploy . --alias my-app \
  -e DATABASE_URL="postgres://user:pass@host:5432/db" \
  -e API_KEY="sk-xxx" \
  -e NODE_ENV=production \
  -e PORT=3000

# 3. Verify it's running
dibbla apps list
```

### Update an existing app (zero downtime)

```bash
# Rolling update — re-deploys the code, keeps existing env vars
dibbla deploy . --alias my-app --update

# To change env vars without redeploying code
dibbla apps update my-app -e LOG_LEVEL=debug -e NEW_VAR=value
```

### Deploy-or-update pattern

```bash
# Check if app exists, then deploy or update accordingly
if dibbla apps list 2>/dev/null | grep -q "my-app"; then
  dibbla deploy . --alias my-app --update
else
  dibbla deploy . --alias my-app \
    -e DATABASE_URL="postgres://..." \
    -e NODE_ENV=production
fi
```

### Full setup: app + database + secrets

```bash
# 1. Create the database (scoped to the deployment).
#    When --deployment is set, the auto-created secret is named
#    DATABASE_URL_<UPPERCASED_NAME>, e.g. DATABASE_URL_MY_APP_DB here.
#    Without --deployment it is named DATABASE_URL.
dibbla db create my_app_db --deployment my-app

# 2. Set additional secrets
dibbla secrets set API_KEY "sk-xxx"

# 3. Deploy with env vars
dibbla deploy . --alias my-app \
  -e API_KEY="sk-xxx" \
  -e NODE_ENV=production

# 4. Verify
dibbla apps list
dibbla secrets list -d my-app

# Alternative: get the connection string directly
export DATABASE_URL=$(dibbla db connect my-app-db -q)
```

### Tear down an app

```bash
# Always use --yes to avoid interactive prompt
dibbla apps delete my-app --yes
dibbla db delete my-app-db --yes
dibbla secrets delete API_KEY --yes
```

### Install a template and start iterating

```bash
# 1. Show available templates to the user
dibbla template list

# 2. Install the one they picked (default destination — ./<template_path>)
dibbla template install expense-reporter

# 3. The bootstrap yaml does the rest automatically:
#    - tool checks (git, node, go, dibbla)
#    - dibbla self-update (auto-installs latest dibbla)
#    - clones the template project into CWD
#    - dibbla login via env-var (DIBBLA_AUTH_SERVICE_URL picked up from parent)
#    - npm install, go build, start dev servers (ports per template)
#    - opens the app in the default browser
#
#    End state: a live local project the user can edit.

# 4. Iteration: re-run the inner dibbla-task.yaml any time
cd expense-reporter-template-1
dibbla run
# idempotent — existing installs skip, stale dev servers are reclaimed
```

### Run a bootstrap yaml directly (without dibbla template install)

```bash
# Same end state, one command:
mkdir my-app && cd my-app
dibbla run https://raw.githubusercontent.com/dibbla-agents/dibbla-public-templates/master/getting-started.dibbla-task.yaml
```

## Workflows

These three transcripts cover the lifecycle: build a new workflow from scratch, iterate on it via patches, and roll back when something goes wrong. The full mental model is in [workflows.md](workflows.md); this page is for copy-paste shape.

### Build an agent + tool workflow from scratch

The canonical "ask an LLM a question, let it call a weather tool" shape. Validate before sending, smoke-test after.

```bash
# 1. Discover what's available — registry, not YAML, is the source of truth
dibbla fn list --tag accepts_tools                  # agent-eligible functions
dibbla fn list --server go-function-server1         # all functions on the server
dibbla fn get go-function-server1 reasoning_agent_function   # full schema

# 2. Author the slim YAML
cat > /tmp/weather.yaml <<'EOF'
name: weather_assistant
label: "Weather Assistant"
description: "Asks an LLM the weather, with one tool wired in"
nodes:
  - id: api_input
    type: api
    inputs: [question]
    outputs: [question]

  - id: system_prompt
    type: function
    function: handlebars_template
    server: go-function-server1
    inputs:
      script: |
        You are a helpful weather assistant.
        Use the get_weather tool whenever the user asks about a location.
    outputs: [error, output]

  - id: agent
    type: function
    function: reasoning_agent_function
    server: go-function-server1
    inputs:
      model: "claude-sonnet-4-5-20250514"
      prompt_message: ~
      system_message: ~
    tools:
      - weather_tool
    outputs: [response]

  - id: weather_tool
    type: function
    function: get_weather_function
    server: go-function-server1
    inputs:
      query: ~
    outputs: [result]

  - id: api_response
    type: api_response
    linked_to: api_input
    inputs: [response]

edges:
  - api_input.question -> agent.prompt_message
  - system_prompt.output -> agent.system_message
  - agent.response -> api_response.response
EOF

# 3. Validate — safe, never persists
dibbla wf validate -f /tmp/weather.yaml

# 4. Create on HEAD
dibbla wf create -f /tmp/weather.yaml

# 5. Print the HTTP endpoint and a curl example
dibbla wf api-docs weather_assistant

# 6. Smoke-test from the CLI
dibbla wf execute weather_assistant --data '{"question":"What is the weather in Berlin?"}'

# 7. Snapshot the working version before any edits
dibbla revisions create weather_assistant
```

### Today's date in the system prompt (no cycle)

A frequent need: inject "today's date" (or any other live value) into an agent's system prompt. The shape that does **not** create a cycle is `data_source -> handlebars_template -> agent.system_message`, with the data source kept out of the agent's `tools:` list.

```yaml
name: dated_assistant
nodes:
  - id: api_input
    type: api
    inputs: [message]
    outputs: [message]

  - id: today
    type: function
    function: todays_date
    server: function-server
    inputs: { triggered: true }    # native YAML bool — not "true"
    outputs: [date]

  - id: prompt
    type: function
    function: handlebars_template
    server: function-server
    inputs:
      script: "Today is {{date}}.\n\nYou are a helpful assistant. Answer the user's message clearly and briefly."
      date: ~                       # filled at runtime by today.date
    outputs: [output]

  - id: agent
    type: function
    function: reasoning_agent_function
    server: function-server
    inputs:
      model: "claude-sonnet-4-5-20250514"
      system_message: ~
      prompt_message: ~
    # NOTE: `today` is NOT listed in tools: — it's a pure data input.
    # Listing it here AND wiring today.date -> prompt.date would create
    # the tool+data-edge cycle described in workflows.md §6.

  - id: api_response
    type: api_response
    linked_to: api_input
    inputs: [response, error]

edges:
  - api_input.message -> agent.prompt_message
  - today.date        -> prompt.date
  - prompt.output     -> agent.system_message
  - agent.response    -> api_response.response
  - agent.error       -> api_response.error
```

Verify with a follow-mode smoke test, which surfaces watchdog WARNs immediately if anything fails to fire:

```bash
dibbla wf create -f /tmp/dated_assistant.yaml
dibbla wf execute dated_assistant --data '{"message":"What date is it?"}' --follow
```

### Iterate on an existing workflow with patches

Smaller changes are faster as patch operations against HEAD than as full updates. Each command applies one operation; pair the sequence with `revisions create` snapshots so you can roll back.

```bash
# Always snapshot before a patch sequence
dibbla revisions create weather_assistant

# Add a date tool (a plain function node — no inputs needed)
dibbla nodes add weather_assistant --inline '{
  "id":"date_tool",
  "type":"function",
  "function":"todays_date",
  "server":"go-function-server1",
  "outputs":["date"]
}'

# Or from a file when the spec is bigger
cat > /tmp/news_tool.json <<'EOF'
{
  "id": "news_tool",
  "type": "function",
  "function": "news_search_function",
  "server": "go-function-server1",
  "inputs": {"query": null},
  "outputs": ["headlines"]
}
EOF
dibbla nodes add weather_assistant -f /tmp/news_tool.json

# Wire the date_tool's output into the agent's system message
# (the existing edge from system_prompt → agent.system_message must be removed first
# — an input port can only have one incoming edge)
dibbla edges remove weather_assistant "system_prompt.output -> agent.system_message"
dibbla edges add    weather_assistant "date_tool.date -> agent.system_message"

# Pin a specific model on the agent (overrides the YAML hardcode without rewriting it)
dibbla inputs set weather_assistant agent model "claude-sonnet-4-5-20250514"

# Attach the new tools to the agent — these are by node id, not function name
dibbla tools add weather_assistant agent date_tool
dibbla tools add weather_assistant agent news_tool

# List edges to confirm the wiring
dibbla edges list weather_assistant

# Snapshot the new known-good state
dibbla revisions create weather_assistant

# Re-run the smoke test
dibbla wf execute weather_assistant --data '{"question":"What is the weather in Berlin today?"}'
```

### Roll back a bad change

When a patch sequence broke something, restore the most recent good revision. Note that `restore` overwrites HEAD — it is not a checkout you can return from without re-restoring.

```bash
# See available revisions, newest first
dibbla revisions list weather_assistant
# ID     TIMESTAMP             LABEL
# 87f3   2026-05-05T13:42:18Z
# 1td9   2026-05-05T13:08:42Z
# 9k3q   2026-05-04T18:11:00Z

# Snapshot the (broken) HEAD before restoring — so you can recover the
# broken state if you change your mind, or diff to understand what went wrong
dibbla revisions create weather_assistant

# Restore the known-good revision; HEAD now equals 1td9
dibbla revisions restore weather_assistant 1td9

# Confirm by re-running the smoke test
dibbla wf execute weather_assistant --data '{"question":"What is the weather in Berlin?"}'

# If you need a copy of the broken state for inspection without affecting HEAD:
dibbla wf get weather_assistant --revision <broken_id> -o yaml > /tmp/broken.yaml
```

### Calling a workflow from production code

Two paths exist; pick the right one. From a Dibbla-deployed app with a `WORKFLOW_API_KEY` (an `ak_…` Bearer token), use the **gateway URL** — not the URL `wf api-docs` prints. The api-docs URL is the workflow-server's internal/direct address and rejects `ak_` tokens at the gateway with "Missing authentication headers."

```bash
# What `wf api-docs` shows (internal — do NOT paste into production code):
#   https://workflow-server.dibbla.net/api/execute/weather_assistant/ni3xl724
# What production code should call (gateway, accepts `ak_` Bearer):
#   https://api.dibbla.net/api/wf/execute/weather_assistant/ni3xl724

# Get the urlid from api-docs, then rewrite the host
URL_ID=$(dibbla wf api-docs weather_assistant -o json | jq -r '.url_id')
GATEWAY_URL="https://api.dibbla.net/api/wf/execute/weather_assistant/${URL_ID}"
echo "$GATEWAY_URL"
```

A robust JavaScript caller. Always use `AbortController` with a short timeout — Node's undici defaults to a 5-minute headers timeout, and a stale `<urlid>` (see footguns) hangs silently for the full 5 minutes before throwing `UND_ERR_HEADERS_TIMEOUT`. Log before and after so failures don't look like hangs.

```javascript
// One workflow call, fail-fast and visible in logs.
async function askWeatherAgent(question) {
  const url = `https://api.dibbla.net/api/wf/execute/weather_assistant/${process.env.WEATHER_WF_URL_ID}`;
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), 60_000); // 60s — workflow calls behave like external HTTP

  console.log("[wf] → weather_assistant", { question });
  const t0 = Date.now();
  try {
    const r = await fetch(url, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${process.env.WORKFLOW_API_KEY}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ question }),
      signal: ctrl.signal,
    });
    console.log("[wf] ← weather_assistant", { status: r.status, ms: Date.now() - t0 });
    if (!r.ok) throw new Error(`weather_assistant returned ${r.status}: ${await r.text()}`);
    const json = await r.json();
    // Always check the agent's error field — see workflows.md §6 on silent-empty agents
    if (json.error) throw new Error(`weather_assistant error: ${json.error}`);
    return json.response;
  } catch (err) {
    if (err.name === "AbortError") throw new Error("weather_assistant timed out after 60s");
    throw err;
  } finally {
    clearTimeout(timer);
  }
}
```

If the gateway call hangs while `dibbla wf execute weather_assistant --data '…'` against the same workflow works, the `<urlid>` has gone stale after too many `wf update` iterations. Recreate the workflow before debugging auth/path/header issues:

```bash
# Snapshot first so you can inspect the old YAML if needed
dibbla wf get weather_assistant -o yaml > /tmp/weather_pre_recreate.yaml
dibbla wf delete weather_assistant --yes
dibbla wf create -f /tmp/weather_pre_recreate.yaml
dibbla wf api-docs weather_assistant      # ← fresh urlid; update WEATHER_WF_URL_ID env in the app
```
