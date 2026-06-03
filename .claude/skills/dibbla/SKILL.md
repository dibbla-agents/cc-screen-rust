---
name: dibbla
description: Dibbla CLI for scaffolding projects, deploying apps, managing apps/databases/secrets and multi-service `dibbla.yaml` manifests, authoring and operating workflows (slim YAML; `wf execute` sync/`--async`/`--follow`; `wf logs <runId>`, `wf runs list/output` for run monitoring), building Go workers via `github.com/dibbla-agents/sdk-go`, and running `dibbla-task.yaml` pipelines (`dibbla run`).
when_to_use: Trigger on Dockerfile/.dibblaignore/deploy-readiness, build-time vs runtime env vars (Vite/Next/CRA), Postgres TLS, `X-User-*` auth headers, Google OAuth, 524 on deploy, BUILD_FAILED, dibbla.yaml multi-service manifest (services, jobs, env-aware fields, profiles, service discovery `DIBBLA_SVC_*`, `expose_to`/NetworkPolicy, init containers, healthchecks, cron, build secrets, ALIAS_HOSTNAME_COLLISION, `manifest validate`, `dibbla preview`, `apps restart --service`), stateful services + TCP routes (`stateful: true`, `routes:`, MongoDB/Redis/RabbitMQ on a public TLS endpoint, IngressRouteTCP, STATEFUL_NO_VOLUME, ROUTE_INVALID, `tls: edge`/`passthrough`, SNI routing, why Postgres/MySQL TCP routes don't work in v1, `dibbla apps delete` is destructive on PVCs), AI gateway (`ai.dibbla.net`, `DIBBLA_AI_GATEWAY_URL`, `X-Dibbla-App` header, OpenAI/Anthropic SDK base URL swap, per-user/per-app LLM call attribution), workflow authoring (slim YAML, agent+tool wiring, UNSATISFIED_INPUT/UNKNOWN_FUNCTION, revisions, functions registry, HTTP execution), workflow run monitoring (`wf execute --async`/`--follow`, `wf logs <runId> --follow`, `wf runs list`, `wf runs output`, `run_completed` sentinel), sdk-go Go workers (`sdk.New`, `RegisterFunction`, `RegisterJob`, SimpleFunction vs advanced Function, JobHandler progress, `gs.OAuth`, `internal/state` import footgun), login from CI/SSH/Docker (`--browser`/`--api-key`/`--write-env`/`--no-keychain`), and `dibbla uninstall`.
---

# Dibbla CLI

The `dibbla` CLI scaffolds projects and manages **applications**, **databases**, **secrets**, and **workflows** on the Dibbla platform. Deployed apps are available at `https://<alias>.dibbla.com`.

## Prerequisites

**Install the CLI** if it isn't already on the user's `PATH`:

| Platform | Command |
|----------|---------|
| macOS (Homebrew) | `brew install dibbla-agents/tap/dibbla` |
| macOS / Linux (shell installer) | `curl -fsSL https://install.dibbla.com/install.sh \| sh` |
| Windows (PowerShell) | `powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://install.dibbla.com/install.ps1 \| iex"` |
| Verify | `dibbla --version` |

The shell installer drops the binary into `~/.local/bin` and adjusts `PATH` if needed. Self-update is available inside task files via the same installer URL.

**Deploying requires a `Dockerfile`** at the root of the directory you pass to `dibbla deploy`. The CLI does **not** auto-detect languages or generate a Dockerfile — if it's missing, the backend rejects the build with log output. All bundled templates in `dibbla-agents/dibbla-public-templates` ship a working Dockerfile you can copy (typically multi-stage: Node → JS build → Go → binary → small runtime image, `EXPOSE 80`).

## Commands at a glance

| Area       | Commands |
|------------|----------|
| Run        | `run [path\|url]`, `run --preview`, `run --env KEY=VAL`, `run --env-file <file>`, `run --work-dir <dir>`, `run --format plain\|gh` |
| Template   | `template list [--refresh] [-v]`, `template install <id> [<dir>] [--force]` |
| Skills     | `skills list`, `skills install <id>` (`--user`, `--force`, `--no-agents`) — install AI-agent guidance into `.claude/skills/` + `AGENTS.md` + `GEMINI.md` |
| Setup      | `init` (interactive setup wizard: update → login → install dibbla skill), `update [--check] [--version vX.Y.Z]` (self-update; defers to brew/apt/rpm/scoop/choco when one owns the binary), `uninstall [--dry-run] [--keep-config] [--keep-skills] [--skill-only]` (removes binary on script installs, keychain creds, `~/.config/dibbla/`, `~/.dibbla/`, and skill files at every recorded install root; for package-manager installs prints the native uninstall command instead of touching the binary) |
| Login      | `login [api_url]`, `login --browser`, `login --api-key <token>`, `login --api-url <url>`, `login --write-env`, `login --no-keychain`, `logout` |
| Status     | `status` (CLI version + resolved API URL with source + token presence with source + live token validation), `status --no-validate` (skip the network call), `status --json` (machine-readable). Exits `0` when logged in or token is configured + validation skipped, `3` when not logged in or token rejected |
| Feedback   | `feedback <message>`, `feedback list`, `feedback delete <id>` |
| Deploy     | `deploy [path] -m "<msg>" [--alias name] [--update] [--require-login] [--access-policy] [--google-scopes] [--target-env <env>] [--profile <p>]` — deploy from directory; `-m` becomes the VCS commit subject. `--target-env` / `--profile` / `--no-public` only apply when a `dibbla.yaml` is at the deploy root |
| Clone      | `clone <app> [--ref <sha>] [--into <dir>]` — clone the Dibbla-managed git repo for a deployed app (read-only; push is rejected) |
| Manifest   | `manifest validate [path]` — local schema check for `dibbla.yaml` (no network) |
| Preview    | `preview [path] [--target-env <env>] [--profile <p>] [--no-public]` — server-authoritative dry run; full env-aware resolution + quota check, no build, no apply |
| Apps       | `apps list`, `apps update <alias>`, `apps delete <alias>`, `apps restart <alias> --service <name>` (per-service rolling restart) |
| Logs       | `logs <app>` (last 15m, **merged across all services in the deployment** by default), `logs <app> --since 24h`, `logs <app> -f` (follow), `logs <app> -n 200` (tail), `logs <app> --grep <regex>`, `logs <app> --json` — runtime logs from Loki; **omit `--service` for accumulated deployment-wide logs**, add `logs <app> --service <name>` to filter to one service; `logs <app> --service <name> --pod-stream` streams pod logs via the K8s API when Loki isn't available |
| Db         | `db list`, `db create`, `db delete`, `db dump`, `db restore`, `db connect` |
| Secrets    | `secrets list`, `secrets set`, `secrets get`, `secrets delete` (global, `-d <alias>` for deployment-wide, or `-d <alias> --service <name>` for per-service) |
| Admin      | `admin reconcile` — force one orphan-resource sweep on the deploy-api instance (gated by `DIBBLA_ADMIN_TOKEN`) |
| Workflows  | `workflows list`, `get`, `create`, `update`, `delete`, `validate`, `execute [--async\|--follow]`, `url`, `api-docs`, `logs <runId> [-f]` |
| Runs       | `wf runs list [--workflow <name>] [--limit <N>]`, `wf runs output <runId>` — list past runs and fetch the api_response payload of a finished run |
| Nodes      | `nodes add <wf>`, `nodes remove <wf> <id>` |
| Edges      | `edges add <wf> "<edge>"`, `edges remove`, `edges list` |
| Inputs     | `inputs set <wf> <node> <input> <value>` |
| Tools      | `tools add <wf> <agent> <tool>`, `tools remove` |
| Revisions  | `revisions list <wf>`, `revisions create`, `revisions restore` |
| Functions  | `functions list`, `functions get <server> <name>` |

## Agent guidelines

**Interactive prompts:** The following commands prompt for confirmation and will block if run non-interactively. Always pass `--yes` (or `-y`) when running these as an agent:
- `dibbla apps delete <alias> --yes`
- `dibbla db delete <name> --yes`
- `dibbla secrets delete <name> --yes`
- `dibbla workflows delete <name> --yes`
- `dibbla nodes remove <wf> <id> --yes`
- `dibbla feedback delete <id> --yes`

**Deploying an app for the first time:**
1. Check if the app already exists: `dibbla apps list`
2. If it does **not** exist, deploy with all required environment variables included in the deploy command — there is no app to attach them to yet:
   ```bash
   dibbla deploy . --alias my-app -m "feat: initial deploy" \
     -e DATABASE_URL=postgres://... -e API_KEY=secret -e NODE_ENV=production
   ```
3. If it **already** exists, use `--update` for a zero-downtime rolling update:
   ```bash
   dibbla deploy . --alias my-app -m "fix: resolve 500 on /search" --update
   ```
   To change env vars on an existing app, use `apps update` instead:
   ```bash
   dibbla apps update my-app -e NEW_VAR=value
   ```

**Key rules:**
- **Every `dibbla deploy` must include `-m "<message>"`.** The value becomes the git commit subject in the app's Dibbla-managed VCS history (and on the GitHub mirror, if configured). Treat it like a git commit: present-tense imperative, under ~72 chars, covering what changed and why — e.g. `-m "fix: handle null org in /api/me"`, `-m "feat: add nightly db backup workflow"`, `-m "chore: bump node to 20.14"`. For retries or mechanical redeploys, still say so explicitly: `-m "redeploy: retry after CF 524"`. Max 500 chars. Never run `dibbla deploy` without `-m`; a blank deploy history is a bug, not a default.
- `--force` causes downtime (tears down and redeploys). Prefer `--update` for existing apps.
- `--force` and `--update` are mutually exclusive.
- Environment variables set via `deploy -e` or `apps update -e` persist across updates — you only need to pass them once.
- **Login guard:** Use `--require-login` to require authentication. Combine with `--access-policy invite_only` to restrict to invited users, or `all_members` for org-wide access. Use `--google-scopes` to request additional Google OAuth scopes (e.g. Drive, Calendar).
- Use `--quiet` / `-q` on `db list`, `db delete`, `db connect` for machine-readable output in scripts.
- `db create --deployment <alias>` scopes the database and its auto-created secret to a specific deployment. The scoped secret is named `DATABASE_URL_<UPPERCASED_UNDERSCORED_NAME>` (e.g. `DATABASE_URL_MY_DB` for database `my_db`), **not** a plain `DATABASE_URL` — app code must read the suffixed env var.
- `db connect` prints a psql-compatible connection string via the Dibbla database proxy. Use `-q` for scripting: `psql $(dibbla db connect mydb -q)`.
- **524 on deploy ≠ failure.** `dibbla deploy` holds a single HTTP connection during the backend build; builds over ~100s may return a Cloudflare 524 on the client even when the backend succeeds. Wait 2–5 minutes, then run `dibbla apps list` to check. Do **not** retry with `--force` — use `--update` if you must retry.
- **Output modes:** `dibbla deploy` streams a live buildkit-style step view when stdout is a TTY and switches to ISO-timestamped log lines (`<ts> [info] build step=N/M …`) when stdout is piped or in CI. Add `--quiet` for a single-line success/failure (script-friendly) or `--json` for a single structured object on stdout. On build failure the non-TTY mode also writes one structured JSON line to **stderr** with shape `{"event":"deploy.failed","step":"go-build","step_index":N,"step_count":M,"errors":[{file,line,col,message}],"retry_cmd":"…","api_error_code":"BUILD_FAILED"}` — coding agents should read this from stderr to locate failing files without scraping the human-readable build output. Add `--verbose-build` to ship the full server build log instead of the elided tail when parsed compile diagnostics aren't enough. Build failures exit `2`; other errors exit `1`.
- **`.dibblaignore` controls Dibbla's managed VCS history**, not what the Docker build sees. The backend always strips `.env`, `node_modules/`, `dist/`, `*.pem`, `*.key` and similar from VCS and reports each hit in `DeployResponse.vcs_filtered` as a warning. Adding those paths (or any generated/large artifact) to `.dibblaignore` at the deploy root silences the warning and keeps VCS clean. Per-file and per-commit size caps are hard rejections — committing a large build artifact will fail the deploy with `ErrCodeVCSFiltered`; the fix is to add the path to `.dibblaignore`. Full details in `reference.md` → deploy → `.dibblaignore`.
- Managed Postgres uses a **self-signed TLS cert**. App clients (pg, psycopg2, Prisma) need explicit SSL handling — see `reference.md` "TLS for application database clients" for working snippets.

**Designing a multi-service manifest:** Before authoring a `dibbla.yaml`, work through these design questions and confirm a plan with the user. Skipping this step at design time leads to retrofits that touch every consumer service (env vars, `depends_on`, service-discovery references), so it's worth the 60 seconds upfront.

- *Which services should exist in only some envs?* (e.g. an inline DB container in dev, a managed/external DB in prod) → put `profiles: [dev]` on the env-specific service. Decide this **upfront** because consumers will need env-aware values for the URL/host that points to it.
- *Which fields differ across envs?* (`replicas`, `image`, `MONGO_URL`, `LOG_LEVEL`, …) → use **env-aware** field maps (§ 6 in [manifest.md](manifest.md)). Different mechanism from profiles: profiles toggle whether a service exists at all; env-aware fields shape an existing service.
- *Where will the data layer live in prod?* If managed/external, the consumer needs an env-aware `MONGO_URL` / `DATABASE_URL` (`default:` → external value, `dev:` → `${DIBBLA_SVC_*}`) **and** the inline copy needs `profiles: [dev]`. The two mechanisms are paired — see § 7 in [manifest.md](manifest.md) for a worked example.
- *How will the user iterate locally?* The platform does **not** run `dibbla.yaml` locally — there is no `dibbla up`. Mirror the manifest into a `docker-compose.yml` next to it for tight inner-loop dev (see [examples.md](examples.md) "Local iteration with docker-compose"). The two diverge in details (no `${DIBBLA_SVC_*}`, no NetworkPolicy, no env-aware resolution) but match in shape.
- *Will any public service be sensitive in prod?* (admin UIs, debug consoles, mail catchers, internal dashboards) → per-service `auth:` block with `require_login: true` and an `access_policy:`, or gate the whole service with `profiles: [dev]`. Shipping an admin UI publicly without auth is a top OWASP-class mistake; the guardrails check enforces this in [guardrails.md](guardrails.md).
- *Are any services stateful — databases, brokers, anything with on-disk state?* If yes, set `stateful: true` and declare at least one `volumes:` entry. The renderer switches to a StatefulSet + headless Service so each pod has stable identity and its own PVC. To expose the service to clients outside the cluster (a `mongosh` from your laptop, an external Redis client), add a `routes:` entry with `type: tcp` + `tls: edge`. **Limit:** TLS-on-connect protocols only — Mongo, Redis-with-TLS, AMQPS, NATS-with-TLS, Kafka-with-TLS — Postgres and MySQL use STARTTLS-style upgrades that don't carry SNI in the first packet, so they're deferred. **Footgun:** `replicas > 1` on a stateful service yields N independent pods each with its own PVC and its own data; the platform does **not** bootstrap clustering protocols. Use `replicas: 1` unless you're wiring clustering yourself. Full schema in [manifest.md § 10.5](manifest.md).

**Pre-deploy guardrails (CLI-enforced):** Before calling `dibbla deploy`, you MUST complete the pre-deploy checklist and present findings to the user. Always wait for explicit user confirmation before deploying or fixing issues — never deploy autonomously. The guardrails workflow writes a `REVIEW.md` file to the project root, and **`dibbla deploy` will refuse to upload without it** (and without a user handbook). The platform also reads `REVIEW.md` and displays a review status indicator in the dashboard. The `--skip-review` flag exists for humans making one-line fixes; agents must run the checklist instead of using the flag. See [guardrails.md](guardrails.md) for the full checklist.

**User handbook (MANDATORY, CLI-enforced).** Every deployable app MUST ship user-facing documentation. `dibbla deploy` refuses to upload unless the project root contains **either** a `docs/` folder with at least `docs/index.md`, **or** a single `APP.md` at the root. If neither exists, do **not** deploy — generate a starter handbook from the templates in [user-docs.md](user-docs.md), ask the user to confirm content, then deploy. The handbook is for the **end user of the deployed app** — not developers, not operators. It is rendered in `auth.dibbla.net` under "My Apps → {alias}" and is the only documentation surface end users see. Never put dev-stack notes, framework names, deploy commands, env-var lists, infra details, or other technical metadata in there. See [user-docs.md](user-docs.md) for tone, file layout, cross-linking syntax, paste-ready templates, and the full list of what NOT to include.

**Workflows:** A workflow is a typed DAG of function calls — nodes name a `function` from the registry, edges carry data port-to-port, an `api` node + `api_response` node make it callable over HTTP. Author in **slim YAML** (the format `wf get`/`wf create -f` consume); never hand-write the verbose React-Flow JSON. Minimal shape:

```yaml
name: my_workflow
nodes:
  - {id: api_input,  type: api,          inputs: [question], outputs: [question]}
  - {id: greet,      type: function,     function: handlebars_template,
     server: go-function-server1,        inputs: {script: "Hello {{question}}!"},
     outputs: [error, output]}
  - {id: api_response, type: api_response, linked_to: api_input, inputs: [response]}
edges:
  - api_input.question -> greet.question
  - greet.output -> api_response.response
```

Before authoring anything non-trivial, run `dibbla fn list` to see what functions exist and `dibbla wf get <existing> -o yaml` on a similar workflow for shape — the function registry, not the YAML, is the source of truth. Pick the iteration loop that matches the change size: small tweak → patch HEAD with `nodes add`/`edges add`/`inputs set`/`tools add`; structural change → `wf get … -o yaml` → edit → `wf update -f`. Always `dibbla revisions create <wf>` before either; patches are not auto-snapshotted and `revisions restore` overwrites HEAD (it's not a checkout). For the complete model — node-type roles, the agent+tool pattern, all 13 validator errors and their fixes, execution/HTTP semantics, and the three canonical workflow shapes (transform, agent+tools, multi-stage pipeline) — see [workflows.md](workflows.md).

**Workflow gotchas that bite once:**
- **Pick `reasoning_agent_function` for new agents** — `reasoning_agent_with_thread` has been observed to silently return empty responses with current Claude models. Always wire `agent.error -> api_response.error` so silent failures surface.
- **One node, one role: never both tool AND data input.** If a node feeds an agent's `tools:` list AND has a data edge into the same agent (or transitively), the auto-generated tool-connection edge plus your data edge close a cycle — pre-flight refuses the run with `422 CYCLE_DETECTED`. Pick one role; the canonical "inject this into the system prompt" shape is `data -> handlebars_template -> agent.system_message`, with the data source NOT in `tools:`. See workflows.md §6 and the worked example in examples.md.
- **Use `--follow` for the first execution after any workflow change.** Silent function failures used to surface only after the 30-minute server timeout. The stuck-run watchdog now emits a WARN-level `run is not making progress` within ~30s with a per-input `diagnosis` field — visible immediately in `dibbla wf execute … --follow`. Treat `--follow` as the smoke-test default, not a debugging escape hatch.
- **`wf update` is no longer last-write-wins.** The CLI sends `If-Match` with the current ETag automatically; concurrent edits return `412` with `current_etag`/`received_etag` in the body. Pull, merge, retry — or `--force` to overwrite (deletes whatever the other writer just shipped, so use sparingly).
- **YAML types must match the function's reflected Go types.** Send `triggered: true` not `triggered: "true"`; `42` not `"42"`. `dibbla fn get` is now ground truth (`boolean` / `integer` / `float` / `string`); older cached YAML may have stringly-typed slots — regenerate from `fn get` when in doubt.
- **Production callers must use the gateway URL**, not the URL `wf api-docs` prints. Rewrite host: `https://workflow-server.dibbla.net/api/execute/<name>/<urlid>` (shown by `api-docs`, internal only) → `https://api.dibbla.net/api/wf/execute/<name>/<urlid>` (gateway, accepts `Authorization: Bearer ak_<workflow-api-key>`).
- **`<urlid>` can go silently stale on `wf update`.** The converter preserves a node's UUID only when its semantic id (function name or label) matches one in the existing workflow. Renaming an api node, swapping its function, or restructuring its `inputs:` can regenerate the id with no warning. Verify with `wf api-docs` after any update; if the url id changed, update production callers (or rebuild + redeploy if it's baked at build time).
- **Node ids collapse to the function name on `wf create`.** Don't pick custom ids; refer to tools by function name.
- **Result cache is 1 hour** on `reasoning_agent_function`. During iterative testing, vary the input or use a `*_no_cache` variant.
- **Always wrap workflow fetches in an `AbortController` with a 30–60s timeout** and log before/after — Node's default 5-minute timeout makes failures look like hangs.
- **`function execution failed` is opaque on its own.** The CLI message is identical for panics, returned errors, and input-deserialization failures. Suspect input-type mismatch first; the actual Go error is in the go-toolserver pod's stdout (`kubectl logs <go-toolserver-pod>`). See workflows.md §11 "Diagnosing a hanging `wf execute`" for the decision tree.

**Run monitoring & async execution:**
- `dibbla wf execute` is **synchronous by default** — it blocks until the workflow's `api_response` node fires (server-side timeout: 30 min). For long-running agent workflows or fire-and-forget batches, use `--async` to get back `response_metadata` immediately while the run continues in background. Tail it later with `wf logs <runId> --follow` and fetch the final output with `wf runs output <runId>`.
- `dibbla wf execute --follow` (`-f`) is the one-liner for interactive debugging: starts the run async, tails live logs to stdout, then prints the api_response payload after the server-emitted `run_completed` sentinel. Exits 0 on completion.
- `dibbla wf logs <runId>` works on any run. Live runs stream until completion; finished runs return historic + sentinel and exit immediately. Persistence policy: WARN/ERROR + the `run_completed` row are persisted; INFO/DEBUG are live-only — a quiet completed run will tail to essentially just `run completed`. For the full transcript of a finished run, use `wf runs output <runId>` instead.
- `dibbla wf runs list` (`-w <name>` to filter, `-n <N>` to page; server caps at 500) is the way to find a recent run id without copy-pasting from the dashboard or the DB.
- **Short flag `-f` differs by command:** on `dibbla logs` (app-logs) and `dibbla wf logs`, `-f` is `--follow`. On `dibbla wf execute`, `-f` is also `--follow` — but `--file` had to give up its short alias and uses `-F` instead. Don't suggest `-f payload.json` for `wf execute`; use `--file payload.json` or `-F payload.json`.

**Building Go workers (sdk-go):** The `github.com/dibbla-agents/sdk-go` Go SDK is how workers register custom **functions** and **jobs** with the platform. A worker is a long-lived gRPC client: `sdk.New(...)` → `server.RegisterFunction(...)` and `server.RegisterJob(...)` → `server.Start()` (which blocks forever). External user modules are restricted to `sdk.NewSimpleFunction[In, Out]` because the advanced `Function[In, Out]` handler signature exposes `internal/types` and `internal/state` — Go's `internal/` rule blocks those imports from any module other than `sdk-go`. The `JobHost` abstraction was removed; jobs register directly via `server.RegisterJob(handler)`. Once the worker is connected, its functions appear in `dibbla functions list` and become callable from workflow YAML by `(server, function)` pair (see [workflows.md](workflows.md) for consumer-side wiring). For the full SDK model — server options, function builders, the `JobHandler` interface, `JobContext` arg helpers, the `Logger` task/progress API, OAuth via `gs.OAuth`, and gotchas — see [sdk-go.md](sdk-go.md).

**Non-TTY / agentic invocation:**
- When running from inside Claude Code's `!` prefix, an agent shell, CI with a browser, or any other non-TTY context, use `dibbla login --browser` instead of bare `dibbla login`. The interactive flow needs stdin for the survey picker; `--browser` skips that and goes straight to browser-based OAuth via a localhost callback. Refuses over SSH (CLI ≥ v1.2.20) — the localhost callback can't reach the user's laptop; use `--api-key` instead.
- For true headless (SSH sessions, cloud VMs, CI runners with no local browser), use `dibbla login --api-key <token>` or set `DIBBLA_API_TOKEN` (and optionally `DIBBLA_API_URL`) env vars — the CLI reads env vars in CI automatically. Get a token at https://app.dibbla.com/api-keys.
- **Cloud VMs / SSH / Docker (no keyring):** From CLI ≥ v1.2.21, `dibbla login --api-key=<t>` automatically falls back to a user-level credentials file at `~/.config/dibbla/credentials.env` (mode 0600) when the OS keyring is unavailable — no `--no-keychain` or `--write-env` flag needed. The file behaves like the keyring (machine-wide, persists across `cd`); subsequent `dibbla *` calls from any directory read from it. Combine with `--write-env` to *also* land creds in `./.env` for project-scoped reads. On older CLIs you still need `--api-key=<t> --api-url=<url> --write-env --no-keychain` to land creds in `./.env` only.
- **`.env` in CWD is read by every command, including `login`.** Put `DIBBLA_API_TOKEN=…` and `DIBBLA_API_URL=https://api.dibbla.net` in `./.env` and every `dibbla` invocation from that directory targets that server and token — no `login` call needed. Shell-exported vars still win over `.env` (godotenv does not overwrite). Requires CLI ≥ v1.2.4.
- `DIBBLA_AUTH_SERVICE_URL` is an internal compat alias for `DIBBLA_API_URL`, injected by the steprunner into child processes launched by `dibbla run`. Users should put `DIBBLA_API_URL` in `.env`; `DIBBLA_AUTH_SERVICE_URL` exists so child processes see the same server via the desktop/steprunner convention name.

**Running task files and templates:**
- **To scaffold a published template, reach for the sugar first:** `dibbla template list` to find the id, then `dibbla template install <id>`. Don't hand-build a `github.com/.../blob/...` raw URL — that's the slow, error-prone path. Drop to `dibbla run <https-url>` only when you genuinely have a raw bootstrap URL that isn't in the manifest. (As a safety net, `dibbla run` auto-rewrites a GitHub `/blob/` web URL to its `raw.githubusercontent.com` form.)
- `dibbla run <path>` executes a `dibbla-task.yaml` pipeline locally. Tool checks, shell commands, background dev servers, and browser-open side effects are all possible — the task file becomes shell under the user's account.
- `dibbla run <https-url>` fetches and executes a yaml from the network. **This is equivalent to `curl | bash`** — only run yamls from sources the user trusts (e.g. `github.com/dibbla-agents/*`). Work-dir defaults to the user's invocation CWD, so bootstrap clones land in the expected directory rather than in a temp dir.
- `dibbla template install <id>` is ergonomic sugar over `mkdir ./<template-path> && cd ./<template-path> && dibbla run <bootstrap-url>`. It refuses if the destination directory exists; pass `--force` to reuse. Use `dibbla template list` to see available ids.
- Prefer `dibbla run --preview` or `dibbla template list` before actually running, so the user can see what will execute.

**Installing this skill into a project (so other agents see it too):**
- `dibbla skills install dibbla` writes the skill files into `./.claude/skills/dibbla/` plus `AGENTS.md` and `GEMINI.md` pointers at the project root. Every major coding agent then picks up the guidance automatically — Claude Code via its native skill path, Cursor/Opencode/Codex/Copilot/Windsurf/Aider via `AGENTS.md` (the 2026 open standard), Gemini CLI via `GEMINI.md`.
- The skill content is embedded in the CLI binary (`go:embed`), so no network is required and the skill version is locked to the CLI version the user has installed. Run `dibbla --version` to see which one.
- Flags: `--user` installs into `$HOME` for machine-wide coverage instead of the current directory; `--no-agents` skips `AGENTS.md` and `GEMINI.md` (Claude Code only); `--force` overwrites skill files that have been edited locally. Unknown files inside `.claude/skills/<id>/` are always preserved.
- The AGENTS.md / GEMINI.md pointer block is marker-delimited (`<!-- >>> dibbla skill >>> -->` … `<!-- <<< dibbla skill <<< -->`) so existing AGENTS.md content outside the markers is preserved byte-for-byte across reruns.
- Re-running is idempotent — if nothing changed, nothing is rewritten (no mtime bump). Use `dibbla skills list` to see what skills the current CLI ships.

## Additional resources

- **AI gateway (deployed apps):** see [ai-gateway.md](ai-gateway.md) for `https://ai.dibbla.net`, the `X-Dibbla-App` attribution header, the auto-injected `DIBBLA_AI_GATEWAY_URL` env var, OpenAI/Anthropic SDK snippets that swap the base URL, and the per-org dashboard. Read this whenever an app needs to call an LLM and the user wants the call audited / attributed under their Dibbla org.
- **AI gateway (laptops & IDE assistants):** for pointing Claude Code, Cursor, opencode, Cline, Windsurf, or Zed at the gateway from a developer machine — and for `dibbla ai url|env|test` — install the dedicated `dibbla-ai-gateway` skill: `dibbla skills install dibbla-ai-gateway` (then read its SKILL.md).
- **Platform compatibility:** see [platform.md](platform.md) for the Dockerfile contract, port-matching, runtime environment, managed Postgres TLS handling, secrets/env-var injection, the auth-header contract (`X-User-*` headers and Google OAuth scope brokering), the upload boundary, the multi-service runtime contract (§ 8.5), and the pre-deploy compatibility checklist. Read this when working in a Dibbla-connected project on Dockerfile, `.dibblaignore`, auth integration, or deploy-readiness questions.
- **Multi-service manifest schema:** see [manifest.md](manifest.md) for the full `dibbla.yaml` schema — services, jobs, env-aware fields, profiles, service discovery (`DIBBLA_SVC_*`), `expose_to`/NetworkPolicy, volumes, **stateful services + TCP routes (§ 10.5)**, init containers, healthchecks, multiple public services, custom domains, cron, build-time secrets, quotas, error codes, and a worked end-to-end example. Read this whenever the user is authoring or reviewing a manifest, or asking a "how do I run X alongside Y in one deploy" question, or "how do I expose a database to my laptop".
- **Workflows:** see [workflows.md](workflows.md) for the complete workflow model — slim YAML format, the three node types and the roles `function` plays (agent / tool / script / data fetcher), the agent+tool wiring pattern, all 13 validator errors with fixes, edges and data flow, the functions registry, the three idiomatic authoring loops, revision semantics, HTTP execution, the canonical workflow shapes, the pre-flight checklist, and footguns. Read this whenever the user asks anything that touches `dibbla wf`/`nodes`/`edges`/`inputs`/`tools`/`revisions`/`functions`.
- **Go SDK:** see [sdk-go.md](sdk-go.md) for the Dibbla Go SDK — `sdk.New` server bootstrap and options, `SimpleFunction` vs advanced `Function[In, Out]`, the `JobHandler` interface and `server.RegisterJob` (the old `JobHost` is gone), the `Logger` task/progress API, OAuth-on-behalf-of-user via `gs.OAuth`, TLS auto-detection, the `internal/` import footgun, and end-to-end deploy. Read this whenever the user is implementing a Dibbla function or job in Go.
- **Full command and flag reference:** see [reference.md](reference.md) for usage, arguments, and all flags.
- **Usage examples:** see [examples.md](examples.md) for copy-paste examples and scripting patterns.
- **Pre-deploy guardrails:** see [guardrails.md](guardrails.md) for the mandatory pre-deploy security checklist (Checks 1–4 always; Check 5 for URL-fetched task files; Check 6 for multi-service manifests; Check 7 user handbook presence).
- **User handbook (end-user docs):** see [user-docs.md](user-docs.md) for the `docs/` folder convention, `_nav.yaml` sidebar ordering, the single-file `APP.md` fallback, audience and tone rules, cross-linking syntax, paste-ready templates (Welcome / Getting Started / FAQ / Features), and the list of things that must **never** appear in user docs (dev stack, env vars, deploy commands, infra). Read this whenever you scaffold a new project, when you're about to call `dibbla deploy`, or when the user asks "how do I document this for end users".

When suggesting or generating `dibbla` commands, use the reference for exact syntax and the examples for typical workflows. For "is my app ready for Dibbla?" questions, start in `platform.md`. For "build / iterate / debug a workflow" questions, start in `workflows.md`. For "implement a Go function/job for the platform" questions, start in `sdk-go.md`.
