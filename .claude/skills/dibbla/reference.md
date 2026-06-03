# Dibbla CLI — Command reference

Complete usage, arguments, and flags for all commands.

---

## login

Authenticate with the Dibbla API and store the token in the OS credential store.

| Item | Details |
|------|---------|
| **Usage** | `dibbla login [api_url]` |
| **Arguments** | `api_url` (optional) — API endpoint (e.g. `api.dibbla.net` or `https://api.dibbla.net`). If omitted, the URL resolves in this order: `$DIBBLA_API_URL` → `$DIBBLA_AUTH_SERVICE_URL` → default `https://api.dibbla.com`. Both env names are read from `./.env` (CWD) as well as the shell environment. |
| **Flags** | `--browser` — skip the interactive menu; go directly to browser OAuth. Works in non-TTY contexts (Claude Code `!` prefix, agent shells) because the flow uses a localhost callback, not stdin. |
|  | `--api-key <token>` — pass a pre-generated token; works in any context |
|  | `--api-url <url>` — explicit API endpoint URL (alternative to the positional arg; **mutually exclusive** with it — specifying both is an error). Useful in long command lines like yaml steps where positional args are easy to miss. |
|  | `--write-env` — after successful validation, write `DIBBLA_API_TOKEN` + `DIBBLA_API_URL` to `./.env` in the current working directory and ensure `.env` is listed in `./.gitignore`. Writes are atomic (tmp-file → rename) and merge in place — existing keys and comments are preserved; only the two DIBBLA keys are replaced. Unix file perms are 0600. Requires CLI ≥ v1.2.4. |
|  | `--no-keychain` — skip *all* machine-wide persistence: neither the OS keyring nor the user-level credentials file (see below) is written. Token is validated only. Combine with `--write-env` to persist to `./.env` instead. From CLI ≥ v1.2.21, plain `dibbla login` already auto-falls back to the user-level file when the keyring is unavailable, so this flag is now mainly used to *opt out* of disk persistence entirely. Requires CLI ≥ v1.2.4. |
|  | **Auto-fallback** (no flag) — from CLI ≥ v1.2.21, when the OS keyring is unavailable (e.g. Linux SSH host without libsecret), `dibbla login` writes credentials to a user-level file at `~/.config/dibbla/credentials.env` (mode 0600). The file is read by every subsequent `dibbla *` invocation regardless of cwd, mirroring keychain semantics. `dibbla logout` and `dibbla uninstall` clean it up. |
| **Interactive** | Real TTY only: picker for "Log in with browser" or "Paste an API token" |
| **Note** | In CI or sandbox sessions, set `DIBBLA_API_TOKEN` (and optionally `DIBBLA_API_URL`) in the shell environment or `./.env` — the CLI reads both, and `login` is not required. Use `DIBBLA_API_URL` as the canonical name; `DIBBLA_AUTH_SERVICE_URL` is an internal compat alias. |

**Canonical flag combinations:**

| Context | Command |
|---|---|
| Laptop (keychain only, default) | `dibbla login` (interactive) or `dibbla login --browser` |
| Laptop with project `.env` as well | `dibbla login --browser --write-env` or `dibbla login --api-key=<t> --write-env` |
| Cloud VM / SSH / Docker (no keyring), CLI ≥ v1.2.21 | `dibbla login --api-key=<t> --api-url=<url>` (auto-falls back to user-level file) |
| Cloud VM / SSH / Docker (no keyring), CLI < v1.2.21 | `dibbla login --api-key=<t> --api-url=<url> --write-env --no-keychain` |
| Bootstrap yaml step (agent-invoked) | `dibbla login --api-key=$DIBBLA_API_TOKEN --api-url=$DIBBLA_AUTH_SERVICE_URL --write-env --no-keychain` |

### logout

| Item | Details |
|------|---------|
| **Usage** | `dibbla logout` |
| **Output** | Removes stored token + api_url from the OS credential store |

---

## status

Print the CLI version, the API server this CLI will talk to, and whether a valid login is configured. Useful for "where is this CLI pointing?" diagnostics — particularly across multiple shells where one has `DIBBLA_API_URL` exported and another doesn't, or after editing `.env`.

| Item | Details |
|------|---------|
| **Usage** | `dibbla status` |
| **Flags** | `--json` — emit a machine-readable JSON report instead of human text |
| | `--no-validate` — skip the live token validation request (report only what's stored locally) |
| **Validation** | When a token is configured, `status` calls `POST /api/auth/v1/tokens/validate` against the resolved API URL so "logged in" reflects the *live* state of the token (revoked / expired tokens show as not logged in). Skip with `--no-validate` for offline use. |
| **Resolution order** | API URL: `DIBBLA_API_URL` > `DIBBLA_AUTH_SERVICE_URL` > keyring > credentials file > default (`https://api.dibbla.com`). Token: `DIBBLA_API_TOKEN` > keyring > credentials file > none. The `source` annotation in the output identifies which won. |
| **Exit codes** | `0` — logged in, or `--no-validate` and a token is configured. `3` — not logged in / token rejected. `1` — unexpected error (network, malformed response). |

**Human output:**

```
Dibbla CLI 1.2.24
API:     https://api.dibbla.com  (default)
Token:   configured  (source: keyring)
Status:  ✅ logged in
```

**JSON shape:**

```json
{
  "version": "1.2.24",
  "api_url": "https://api.dibbla.com",
  "api_url_source": "default",
  "token_configured": true,
  "token_source": "keyring",
  "validated": true,
  "logged_in": true
}
```

`validation_error` is added when a token was rejected; omitted otherwise.

**Examples:**

```bash
dibbla status                          # human text + live validation
dibbla status --no-validate            # offline; skips network
dibbla status --json | jq '.api_url'   # script-friendly endpoint extraction
```

**Agent guidance:** before running anything that depends on a working login (`deploy`, `wf execute`, etc.), you can use `dibbla status --json` to detect a missing/invalid token and surface a clear error rather than waiting for the downstream 401. In CI, `DIBBLA_API_TOKEN` always wins over any cached keyring/file credential — `status` will report `source: env (DIBBLA_API_TOKEN)` so you can confirm CI is using the token you think it is.

---

## run

Run a `dibbla-task.yaml` pipeline locally using the dibbla-tasks steprunner.

| Item | Details |
|------|---------|
| **Usage** | `dibbla run [path-or-url]` |
| **Arguments** | (omitted) — runs `./dibbla-task.yaml` from the current directory |
|  | `<local-path>` — runs the given file (work_dir defaults to the yaml's parent directory) |
|  | `<https-url>` — fetches the yaml (5 MB max, 30 s timeout) and runs it with work_dir = your invocation CWD |
| **Flags** | `--preview` — parse and print the execution plan without running anything |
|  | `--env KEY=VAL` — set/override an env var for all steps (repeatable) |
|  | `--env-file <path>` — load env vars from a `.env`-style file |
|  | `--work-dir <dir>` — override working directory for command steps |
|  | `--format plain\|gh` — output format (default `plain`; `gh` emits GitHub Actions workflow commands) |
| **Env injected into steps** | `DIBBLA_API_TOKEN`, `DIBBLA_AUTH_SERVICE_URL` (both when logged in); `DIBBLA_CMD` (path to the running dibbla binary — used by bootstrap yamls for recursive invocation regardless of PATH state) |
| **Security** | URL-fetched yamls become shell commands on the user's machine. Treat them as `curl \| bash` — only run yamls from trusted sources (e.g. `github.com/dibbla-agents/*`). |
| **Exit code** | `0` on success; `1` on step failure or setup error |

---

## template

Discover and install Dibbla templates from the hosted manifest.

Manifest URL (default): `https://raw.githubusercontent.com/dibbla-agents/dibbla-public-templates/master/templates.json`. Override with `DIBBLA_TEMPLATES_URL` to point at a staging or local manifest.

Cache lives at `~/.dibbla/templates-cache.json`. Resolution is simple: a fresh cache (fetched less than 1 h ago) is used silently; otherwise the CLI fetches the manifest from the URL and rewrites the cache. If the fetch fails (offline, 404, etc.), `dibbla template list / install` returns the error — there is no stale-cache or embedded-fallback tier. Pass `--refresh` to bypass the fresh-cache short-circuit and force a network fetch.

### template list

| Item | Details |
|------|---------|
| **Usage** | `dibbla template list` |
| **Flags** | `--refresh` — force re-fetch of the manifest, bypassing fresh cache |
|  | `-v`, `--verbose` — print the manifest source used (cache / network / embedded) |
| **Output** | Table: `ID  NAME  CATEGORY  DESCRIPTION` |

### template install

| Item | Details |
|------|---------|
| **Usage** | `dibbla template install <id> [<dir>]` |
| **Arguments** | `id` (required) — template slug from the manifest (e.g. `getting-started`, `expense-reporter`, `crm`, `presentation`) |
|  | `dir` (optional) — destination directory; defaults to the manifest's `template_path` for that id (e.g. `./expense-reporter-template-1`) |
| **Flags** | `--force` — overwrite (reuse) the destination directory if it already exists |
|  | `--refresh` — force re-fetch of the manifest before installing |
| **Behavior** | `mkdir` destination → `chdir` into it → run the template's `bootstrap_url`. The bootstrap clones the project subtree from the templates repo into CWD and recursively invokes `dibbla run ./dibbla-task.yaml` inside the cloned directory. |
| **Refuses** | If the destination directory already exists and `--force` is not passed |
| **Exit code** | `0` on success; `1` on any failure (manifest lookup, mkdir, bootstrap pipeline) |

---

## skills

Install AI-coding-agent skills embedded in this CLI into a project (or the user's home dir). Skills are compiled into the binary via `//go:embed` — no network is required, and the installed skill always matches the version of `dibbla` the user has on `PATH`.

Coverage: Claude Code reads `.claude/skills/<id>/SKILL.md` natively (gives a `/<id>` slash command). Cursor, Opencode, Codex, Copilot, Windsurf, Aider, Zed, Warp, and RooCode read `AGENTS.md` at project root (the 2026 open standard). Gemini CLI defaults to `GEMINI.md`, which the install also writes (same content as `AGENTS.md`) so Gemini works without editing `.gemini/settings.json`.

### skills list

| Item | Details |
|------|---------|
| **Usage** | `dibbla skills list` |
| **Output** | Table: `ID  DESCRIPTION` — one row per skill bundled with this CLI version |
| **Note** | The list is version-locked to the binary; upgrade the CLI to get newer skills |

### skills install

| Item | Details |
|------|---------|
| **Usage** | `dibbla skills install <id>` |
| **Arguments** | `<id>` (required) — id from `dibbla skills list` (currently only `dibbla`) |
| **Flags** | `--user` — install into `$HOME` instead of the current working directory |
|  | `--force` — overwrite skill files that have been edited locally. Only the embedded filenames are touched; user-added files inside `.claude/skills/<id>/` are always preserved |
|  | `--no-agents` — skip writing `AGENTS.md` and `GEMINI.md` at the target root (Claude Code only) |
| **Writes** | `<root>/.claude/skills/<id>/{SKILL.md,examples.md,guardrails.md,reference.md}` |
|  | `<root>/AGENTS.md` — marker-delimited pointer block (`<!-- >>> dibbla skill >>> -->` … `<!-- <<< dibbla skill <<< -->`). Content outside the markers is preserved byte-for-byte across reruns |
|  | `<root>/GEMINI.md` — same block, for Gemini CLI's default context filename |
| **Idempotent** | Re-running is safe. Identical bytes are no-ops (no mtime bump). CRLF vs LF line endings on AGENTS.md / GEMINI.md are preserved |
| **Atomic** | Each file is written via temp-file + `rename`. No partial skill dir if the process is killed mid-install |
| **Offline** | Yes — the skill is compiled into the binary; version always matches `dibbla --version` |
| **Exit code** | `0` on success; `1` on conflict without `--force` or any write failure |

**Canonical invocations:**

| Context | Command |
|---|---|
| Project-local install (default) | `dibbla skills install dibbla` |
| Machine-wide (every project sees it) | `dibbla skills install dibbla --user` |
| Claude Code only, no AGENTS.md / GEMINI.md | `dibbla skills install dibbla --no-agents` |
| Restore skill files after local edits | `dibbla skills install dibbla --force` |
| Inside a `dibbla-task.yaml` bootstrap step | `dibbla skills install dibbla` (with `depends_on: ["update-dibbla"]` so the CLI is fresh enough) |

---

## feedback

Send, list, and manage feedback.

| Item | Details |
|------|---------|
| **Usage** | `dibbla feedback <message>` |
| **Arguments** | `message` (required) — all arguments are joined into one message |
| **Output** | `Feedback <id> received. Thank you!` |

### feedback list

| Item | Details |
|------|---------|
| **Usage** | `dibbla feedback list` |
| **Output** | Table: ID, USER, DATE, MESSAGE |

### feedback delete

| Item | Details |
|------|---------|
| **Usage** | `dibbla feedback delete <feedback-id>` |
| **Arguments** | `feedback-id` (required) — the feedback UUID |
| **Flags** | `--yes`, `-y` — skip confirmation |

---

## deploy

Deploy a containerized app from a directory. App URL: `https://<alias>.dibbla.com`.

### What your deploy directory needs

- **A `Dockerfile` at the root.** The CLI does not auto-detect languages, doesn't run buildpacks, and doesn't generate a Dockerfile. If the Dockerfile is missing, the backend rejects the build at the build step with logs in the error. The templates in `dibbla-agents/dibbla-public-templates` all ship a working Dockerfile — copy a pattern from one when scaffolding.
- **Whatever your Dockerfile expects** (e.g. `go.mod` + `main.go` for Go, `package.json` + source for Node, etc.). No minimum file set is enforced by the CLI.
- **An exposed port + entrypoint in the Dockerfile.** The CLI's `--port` flag only tells the platform which container port to route to; the Dockerfile's `EXPOSE` and `CMD`/`ENTRYPOINT` are what actually bind and serve traffic.

### What's excluded from the upload archive

The CLI tar.gz's the deploy directory and excludes a hardcoded list: `.git/`, `node_modules/`, `.env.production`, SSH keys (`.pem`, `.key`, `*_rsa`, `*_dsa`), `.DS_Store`, etc. `.dockerignore` is not read by the CLI (but your templates can still have one — it's honored by the backend's Docker build).

### `.dibblaignore` (server-side VCS filter)

When the deploy archive arrives at the backend, a second filter decides which files get committed to Dibbla-managed version control (the bare repo and optional GitHub mirror tied to the app). **This filter only affects VCS history — it does not change what the Docker build sees.** Files excluded from VCS still ship in the build context.

| Item | Details |
|------|---------|
| **Location** | `.dibblaignore` at the root of the deploy directory (same level as `Dockerfile`). The file itself is committed to VCS — keep it under version control. |
| **Syntax** | gitignore-style globs (powered by `sabhiram/go-gitignore`). Supports `**`, directory suffixes (`build/`), negation, etc. Example: `build/`, `**/*.log`, `coverage/`, `*.tmp`. |
| **Platform denylist (always-on)** | `node_modules/`, `dist/`, `.venv/`, `.git/`, `.env`, `.env.*`, `*.pem`, `*.key`. These are **always** filtered from VCS regardless of `.dibblaignore`, and each hit produces a warning returned in the deploy response under `vcs_filtered`. The CLI surfaces these as a recommendation to add the path to `.dibblaignore` to silence the warning. |
| **Suppressing warnings** | Add a path that hits the platform denylist (e.g. `.env`) to `.dibblaignore` and the warning goes away — same file is still excluded from VCS, but silently. User-ignored entries are checked **before** the platform denylist, so `.dibblaignore` always wins on the warning channel. |
| **Hard rejections** | The server enforces per-file and per-commit size caps. If any file exceeds the per-file cap, or the kept set exceeds the total cap, the **entire deploy fails** with `ErrCodeVCSFiltered` (HTTP 400) and a message naming the offending path. Limits are server-configured (`GitMaxFileSize`, `GitMaxCommitDelta`); typical cause is committing a generated artifact, dataset, or build output. Fix by adding the path to `.dibblaignore`. |
| **Symlinks / non-regular files** | Skipped silently. Only regular files are committed. |
| **Disabled mode** | If the platform's `VCSEnabled` flag is off for an app/org, the filter doesn't run and `.dibblaignore` has no effect. The deploy still succeeds either way. |
| **What deploy returns** | `DeployResponse.vcs_commit` — SHA written for this deploy (empty if tree unchanged or VCS disabled); `DeployResponse.vcs_filtered` — paths the platform denylist excluded; `DeployResponse.vcs_error` — non-empty if the deploy went live but the VCS commit step failed (best-effort side channel). |

**Typical contents:**

```gitignore
# Build outputs
build/
dist/
*.tsbuildinfo

# Test/coverage artifacts
coverage/
.nyc_output/

# Local env (also matched by platform denylist — listing here silences the warning)
.env
.env.local

# Editor / OS noise
.vscode/
.idea/
*.swp
.DS_Store

# Large binaries / datasets the Docker build needs but shouldn't be in VCS
data/*.parquet
fixtures/*.bin
```

**Rule of thumb:** if it's generated, secret, or large, put it in `.dibblaignore`. If the deploy response surfaces a `vcs_filtered` entry on every deploy, add it to `.dibblaignore` to clean up the log.

### Flags

| Item | Details |
|------|---------|
| **Usage** | `dibbla deploy [path]` |
| **Arguments** | `path` (optional) — directory to deploy; default `.` |
| **Flags** | `--alias`, `-a` — custom alias name (default: directory name) |
| | `--message`, `-m` — Deploy message, used verbatim as the VCS commit subject (local bare repo and GitHub mirror). Max 500 chars; API returns 400 if exceeded. **Agents must always pass this** — treat it like a git commit subject (imperative mood, ≤72 chars). |
|| | `--force`, `-f` — force redeploy if alias exists (causes downtime) |
| | `--update`, `-u` — rolling update of existing deployment (zero downtime) |
| | `--env`, `-e` — env var `KEY=value` (repeatable) |
| | `--cpu` — CPU request (e.g. `500m`) |
| | `--memory` — Memory request (e.g. `512Mi`) |
| | `--port` — Container port (e.g. `3000`) |
| | `--favicon` — Favicon URL (e.g. `https://example.com/favicon.ico`) |
| | `--require-login` — Require authentication to access the app |
| | `--access-policy` — Access policy: `all_members` or `invite_only` |
| | `--google-scopes` — Google OAuth scope URL (repeatable) |
| | `--target-env <name>` — Manifest env block to resolve (defaults to `prod` server-side). Only meaningful when a `dibbla.yaml` is at the deploy root. |
| | `--profile <name>` — Activate a manifest profile (repeatable). Skipped services appear in the deploy event stream. |
| | `--no-public` — Allow a deploy with no `public: true` service (worker- or cron-only deploy). |
| **Note** | `--force` and `--update` are mutually exclusive |

### Multi-service path vs single-Dockerfile path

`dibbla deploy` picks the path automatically:

- **Multi-service:** `dibbla.yaml` (or `dibbla.yml`) at the deploy root. The deploy-api parses + resolves the manifest, runs quota, builds every `build:` service in parallel, and applies the rendered K8s graph atomically (rollback-on-failure). The deploy event stream tags each event with the service name; the success response carries a `services:` map describing what got deployed.
- **Single-Dockerfile (legacy):** no manifest at the root. Behaves exactly as before: one `Dockerfile`, `--cpu` / `--memory` / `--port` flags shape the single container, response shape unchanged. Backward compatible byte-for-byte.

The two paths are mutually exclusive; the CLI fails fast (`MANIFEST_AMBIGUOUS`) if both `dibbla.yaml` and `dibbla.yml` are present.

For the manifest schema, env-aware fields, profiles, service discovery, NetworkPolicy, init containers, healthchecks, custom domains, cron jobs, and build-time secrets — see [manifest.md](manifest.md).

### Errors specific to manifest deploys

| Code | Meaning | Fix |
|---|---|---|
| `MANIFEST_AMBIGUOUS` | Both `dibbla.yaml` and `dibbla.yml` are present | Remove one |
| `MANIFEST_INVALID` | Schema violation | Read the `path:` and `detail:` in the response |
| `MANIFEST_UNSUPPORTED` | Reserved top-level key (`volumes`, `networks`, `secrets`, `cron`, `init`) | Use the per-service equivalent |
| `SERVICE_NAME_INVALID` | Service name fails regex `^[a-z][a-z0-9-]{0,29}$` or is reserved | Rename |
| `BUILD_CONTEXT_MISSING` / `DOCKERFILE_MISSING` | Path inside the archive is missing | Check `build.context` / `build.dockerfile` against the archive |
| `PUBLIC_SERVICE_MISSING` | No `public: true` service and `--no-public` not set | Mark a service `public: true` or pass `--no-public` |
| `PUBLIC_MISSING_PORT` | A `public: true` service has no `port:` | Add `port:` |
| `QUOTA_EXCEEDED` | Resolved set exceeds an org quota | Trim `replicas` / `cpu` / `memory` / `volumes`, or talk to the platform operator |
| `BUILD_FAILED` | A build step failed | Check the deploy event log; if it's a missing build secret, run `dibbla secrets set <NAME> <value> -d <alias>` first |
| `DEPLOY_IN_PROGRESS` | Another deploy is in-flight for this alias | Wait or `dibbla apps cancel <alias>` |
| `PATCH_AMBIGUOUS` | `dibbla apps update --replicas N` against a multi-service deploy | Edit `dibbla.yaml` and redeploy with `--update` |
| `ALIAS_HOSTNAME_COLLISION` | Multi-public deploy would produce a hostname `<alias>-<service>.<base>` that another existing alias in the org owns | Rename either deploy |
| `ALIAS_EXISTS` | Alias is already in use; pass `--update` (rolling) or `--force` (recreate) | Pick the right mode |
| `RESERVED_ALIAS` | The chosen alias matches a platform-reserved name | Rename |
| `DEPENDS_ON_UNKNOWN` | `depends_on:` references a non-existent service | Fix the service name reference |
| `VOLUME_UNSUPPORTED` | Top-level `volumes:` block is reserved for a future schema version | Use per-service `volumes:` instead |
| `IMAGE_REGISTRY_DENIED` | `image:` references a registry not on the platform's allowlist | Pull from an allowlisted registry, or push to the org's registry |
| `INVALID_HEALTHCHECK` / `MISSING_HEALTHCHECK` | Healthcheck declaration violates the schema (multiple probes / missing required fields) | See manifest.md § 12 |
| `HEALTHCHECK_FAILED` / `HEALTHCHECK_TIMEOUT` | Probe didn't pass at deploy time | Check pod logs; relax `failure_threshold` / `initial_delay_seconds` for slow boots |
| `SERVICE_NAME_TOO_LONG` | Computed K8s name `{alias}-{service}` exceeds 63 chars | Shorten the alias or service name |

---

## manifest

Local-only schema validation for `dibbla.yaml`. No server roundtrip; useful in CI, pre-commit hooks, and editor integrations.

### manifest validate

| Item | Details |
|------|---------|
| **Usage** | `dibbla manifest validate [path]` |
| **Arguments** | `path` (optional) — path to a directory (looks for `dibbla.yaml` / `dibbla.yml`) or a manifest file directly. Defaults to `.`. |
| **Flags** | `--target-env <name>` — informational; resolution runs server-side. |
| | `--profile <name>` — repeatable; informational. |
| | `--no-public` — informational; the local check accepts both. |
| | `--json` — emit a structured JSON report instead of human text. |
| **Exit codes** | `0` valid (or no manifest — legacy single-Dockerfile path); `1` invalid (first error printed). |
| **Local coverage** | Schema version, service-name regex + reserved names, build/image XOR, image-must-have-tag, port range, ambiguous yaml/yml. |
| **NOT covered locally** | Env-aware resolution, quota, multi-public, cross-service references, cycle detection. Use `dibbla preview` for those. |

**Examples:**
```bash
dibbla manifest validate                 # validate ./dibbla.yaml
dibbla manifest validate ./myapp         # validate ./myapp/dibbla.yaml
dibbla manifest validate ./dibbla.yaml   # validate the file directly
dibbla manifest validate --json          # machine-readable
```

---

## preview

Server-authoritative dry run. Uploads the archive, lets the server resolve the manifest + run quota, returns the resolved shape — no build, no apply, no deploy slot consumed.

| Item | Details |
|------|---------|
| **Usage** | `dibbla preview [path]` |
| **Arguments** | `path` (optional) — directory; default `.` |
| **Flags** | `-a`, `--alias <name>` — override directory-name alias. |
| | `--target-env <name>` — manifest env (defaults to `prod` server-side). |
| | `--profile <name>` — repeatable manifest profile. |
| | `--no-public` — allow worker- or cron-only deploys. |
| | `--port <N>` — forwarded as `port` field; used by the no-manifest synthesizer. |
| | `--json` — emit raw `PreviewResponse` JSON. |
| **Exit codes** | `0` valid; `1` invalid (errors printed) or HTTP/transport failure. |
| **Output** | Active services + replica counts, public service name, env-aware-resolved values, skipped services with reason, quota-check result. |

**Examples:**
```bash
dibbla preview                                          # ./, env=prod
dibbla preview ./myapp --target-env staging
dibbla preview --profile mailcatcher --profile metrics
dibbla preview --no-public                              # cron-only deploy is OK
dibbla preview --json | jq '.active_services'
```

---

## apps

### apps list

| Item | Details |
|------|---------|
| **Usage** | `dibbla apps list` |
| **Output** | Table: ALIAS, URL, STATUS, LAST DEPLOYED |

### apps update

| Item | Details |
|------|---------|
| **Usage** | `dibbla apps update <alias>` |
| **Arguments** | `alias` (required) — deployment alias |
| **Flags** | `--env`, `-e` — env var `KEY=value` (repeatable) |
| | `--replicas` — desired replica count |
| | `--cpu` — CPU request/limit (e.g. `500m`, `1`) |
| | `--memory` — Memory request/limit (e.g. `256Mi`, `512Mi`) |
| | `--port` — Container port (1–65535) |
| | `--favicon` — Favicon URL (use `""` to clear) |
| | `--require-login` — Require login: `true` or `false` |
| | `--access-policy` — Access policy: `all_members`, `invite_only`, or `""` to clear |
| | `--google-scopes` — Google OAuth scope URL (repeatable, use `""` to clear) |
| **Rule** | At least one of: `--env`, `--replicas`, `--cpu`, `--memory`, `--port`, `--favicon`, `--require-login`, `--access-policy`, `--google-scopes` required |

### apps delete

| Item | Details |
|------|---------|
| **Usage** | `dibbla apps delete <alias>` |
| **Arguments** | `alias` (required) |
| **Flags** | `--yes`, `-y` — skip confirmation |

### apps restart

Trigger a K8s rolling restart of one service in a multi-service deployment. Idempotent — calling twice in a row produces two pod rollouts. For single-service / legacy deployments, the conventional service name is `app`.

| Item | Details |
|------|---------|
| **Usage** | `dibbla apps restart <alias> --service <name>` |
| **Arguments** | `alias` (required) — deployment alias |
| **Flags** | `-s`, `--service <name>` (required) — service to restart; regex `^[a-z][a-z0-9-]{0,29}$` |
| | `-q`, `--quiet` — print only the alias on success (script-friendly) |
| | `--json` — print the JSON response body |
| **Output** | Default: `✓ rolling restart triggered for <alias>/<service>`. JSON: full response. |
| **Errors** | `404` (service not found) prints a hint to run `dibbla apps list`. `SERVICE_NAME_INVALID` is caught locally before the HTTP call. |

**Examples:**
```bash
dibbla apps restart myapp --service worker
dibbla apps restart myapp -s web --quiet
dibbla apps restart myapp -s redis --json
```

---

## logs

Print runtime logs for a deployed app, sourced from the platform's Loki backend. By default returns the last 15 minutes and exits. **This is the primary way to debug a deployed app without redeploying** — when a deploy succeeds but the app 500s, errors out, or behaves unexpectedly, run `dibbla logs <app>` first rather than adding `console.log` and redeploying.

**Two scopes, controlled by `--service`:**

- **Accumulated / deployment-wide (default — omit `--service`):** returns lines from every service in the deployment, interleaved by timestamp. Each `--json` entry carries a `labels.service` field so you can attribute lines to the originating container. Use this as the entry point — "what is the whole deployment doing?" or "which service is producing this error?".
- **Single service (`-s <name>` / `--service <name>`):** filters server-side to one named service from a multi-service `dibbla.yaml`. Use this once the aggregated view points at a specific service.

For a single-service deployment the two scopes return the same lines.

| Item | Details |
|------|---------|
| **Usage** | `dibbla logs <app>` |
| **Arguments** | `app` (required) — alias of the deployed app whose logs to fetch |
| **Flags** | `--since <duration>` — window to fetch (Go duration; default `15m`, server cap `24h`) |
| | `-f`, `--follow` — stream new lines as they arrive (after the `--since` backfill, if any) |
| | `-n`, `--tail <N>` — show only the last N lines instead of the `--since` window |
| | `--grep <regex>` — server-side regex line filter (LogQL `|~`) |
| | `--limit <N>` — cap lines fetched in range mode (server caps the value) |
| | `--json` — emit raw NDJSON (one Loki entry per line) instead of the human format |
| | `--no-color` — disable colour in the human format (set this for non-TTY callers) |
| | `-s`, `--service <name>` — filter to a single service in a multi-service deployment (forwarded as `?service=`) |
| | `--pod-stream` — stream pod logs via the K8s API instead of Loki (requires `--service`); output is text/plain with `[<pod>] ` line prefixes |
| **Errors** | Returns 404 for apps outside your organisation, or for `--pod-stream` when no pods match the service. Returns 503 if Loki isn't configured (`LOKI_URL` unset) or `--pod-stream` is used and Kubernetes isn't configured. |

### Two log sources

`dibbla logs` has two backends; pick based on what's available and what you need:

| | Loki (default) | K8s pod-stream (`--pod-stream`) |
|---|---|---|
| Server requirement | `LOKI_URL` configured | Kubernetes clientset configured |
| Output format | NDJSON entries with `ts`, `line`, `labels` | text/plain, one `[<pod>] ` line per row |
| Cross-service | Yes (omit `--service` to see all) | No (`--service` is required) |
| Multi-pod | Merged by Loki | Merged client-side; line ordering is per-pod |
| Time range | `--since`, `--tail` | `--tail` only |
| Grep | Yes (LogQL `|~`) | No (filter client-side) |
| Long retention | Yes (Loki retention) | No (just current pod logs) |

**Examples:**

```bash
dibbla logs expense-reporter                         # last 15 min, ALL services merged
dibbla logs expense-reporter --since 24h             # last 24 hours, all services
dibbla logs expense-reporter --since 10m -f          # backfill 10 min, then follow (all services)
dibbla logs expense-reporter -n 200                  # last 200 lines, all services
dibbla logs expense-reporter --grep "timeout"        # server-side filter, all services
dibbla logs expense-reporter --json | jq '{svc: .labels.service, line}'  # attribute lines per service
dibbla logs expense-reporter --service worker -f     # narrow to one service after triage
```

**Agent guidance:** when a deploy succeeds but the app misbehaves, the debugging loop is `dibbla logs <app> --since 30m --grep <error-substring>` → identify the failure → fix code → redeploy with `--update`. Only resort to "redeploy with extra logging" if the existing logs don't surface the issue.

---

## db

### db list

| Item | Details |
|------|---------|
| **Usage** | `dibbla db list [--quiet | -q]` |
| **Flags** | `--quiet`, `-q` — names only, one per line (scripting) |

### db create

| Item | Details |
|------|---------|
| **Usage** | `dibbla db create [name]` or `dibbla db create --name <name>` |
| **Arguments** | `name` (optional as position) — database name |
| **Flags** | `--name` — database name (alternative to argument) |
| | `--deployment <alias>` — scope the database and the auto-created secret to a specific deployment (omit for global) |
| **Rule** | Name required via argument or `--name` |
| **Name rules** | Lowercase letters, digits, and underscores only; must start with a letter; max 63 chars. Pattern: `^[a-z][a-z0-9_]{0,62}$`. Hyphens and uppercase are rejected. |
| **Secret name** | Without `--deployment` the auto-created secret is `DATABASE_URL`. With `--deployment` it is `DATABASE_URL_<UPPERCASED_UNDERSCORED_NAME>` (e.g. `DATABASE_URL_NEXTJS_TODO_DB` for database `nextjs_todo_db`). App code must read the scoped name, not a plain `DATABASE_URL`. |

### db delete

| Item | Details |
|------|---------|
| **Usage** | `dibbla db delete <name>` |
| **Arguments** | `name` (required) |
| **Flags** | `--yes`, `-y` — skip confirmation |
| | `--quiet`, `-q` — errors only (scripting) |

### db dump

| Item | Details |
|------|---------|
| **Usage** | `dibbla db dump <name> [--output <file> | -o <file>]` |
| **Arguments** | `name` (required) |
| **Flags** | `--output`, `-o` — output path; default `<name>.dump` |
| **Output** | Custom-format pg_dump archive |

### db restore

| Item | Details |
|------|---------|
| **Usage** | `dibbla db restore <name> --file <path>` or `-f <path>` |
| **Arguments** | `name` (required) — target database |
| **Flags** | `--file`, `-f` (required) — path to dump file |

### db connect

| Item | Details |
|------|---------|
| **Usage** | `dibbla db connect <name> [--quiet | -q]` |
| **Arguments** | `name` (required) — database name |
| **Flags** | `--quiet`, `-q` — print only the connection string (scripting) |
| **Output** | psql-compatible connection string via Dibbla database proxy. Host and `sslmode` are derived from `DIBBLA_API_URL`: `api.dibbla.com` → `db.dibbla.com` (`sslmode=require`), `api.dibbla.net` → `db.dibbla.net` (`sslmode=disable`, internal), `localhost`/`127.0.0.1` → `sslmode=disable`. Override with `DIBBLA_DB_HOST`, `DIBBLA_DB_PORT`, `DIBBLA_DB_SSLMODE`. Uses API token as password. |

### TLS for application database clients

Dibbla's managed Postgres serves a **self-signed TLS certificate**. Every application client must either relax peer-cert verification or skip it entirely; the connection is still encrypted in transit. Server trust is enforced by network isolation — the database is only reachable from inside the deployment cluster — not by CA-rooted cert identity.

The injected connection string (`DATABASE_URL` or `DATABASE_URL_<NAME>`) already carries an `sslmode` value. If your client reads `sslmode` from the URL *and* accepts explicit SSL options, the URL usually wins — a naive `ssl: { rejectUnauthorized: false }` is silently shadowed. The reliable fix is to **strip `sslmode` from the URL** before passing it to the client, then configure SSL explicitly.

#### Node.js — `pg`

```js
import { Pool } from "pg";

const raw =
  process.env.DATABASE_URL ?? process.env.DATABASE_URL_MY_DB;

const connectionString = raw
  ?.replace(/([?&])sslmode=[^&]*(&|$)/gi, (_, p1, p2) => (p2 ? p1 : ""))
  .replace(/\?$/, "");

export const pool = new Pool({
  connectionString,
  ssl: { rejectUnauthorized: false },
});
```

`ssl: { rejectUnauthorized: false }` alone is not enough — it is overridden by the URL's `sslmode`. Strip `sslmode` first.

#### Python — `psycopg2` / `psycopg`

```python
import os
import psycopg2

conn = psycopg2.connect(
    os.environ["DATABASE_URL"],
    sslmode="require",   # encrypt channel
    sslrootcert="",      # do not require CA verification
)
```

`psycopg` v3 uses the same parameters.

#### Prisma

Append `?sslmode=no-verify` to the URL — a Prisma-specific extension (recent Prisma versions) that accepts self-signed certs without CA verification:

```env
DATABASE_URL="postgresql://user:pass@host:5432/db?sslmode=no-verify"
```

For older Prisma versions that don't recognise `no-verify`, use the `@prisma/adapter-pg` driver adapter and pass `ssl: { rejectUnauthorized: false }` on the underlying `pg` Pool — same pattern as the Node.js snippet above. See Prisma's self-signed-cert docs for version-specific guidance.

---

## secrets

Secrets have three scopes:

- **Global** (no `--deployment`) — visible to every deployment in the org.
- **Deployment-wide** (`--deployment <alias>` or `-d <alias>`, no `--service`) — visible to every service in the deployment.
- **Per-service** (`-d <alias> --service <name>` or `-s <name>`) — visible only to the named service container, not to its peers.

Precedence at deploy time (highest wins): per-service > deployment-wide > global. So a `DATABASE_URL` set globally can be overridden for one deployment by `dibbla secrets set DATABASE_URL ... -d myapp`, and that in turn can be overridden for the worker container by `dibbla secrets set DATABASE_URL ... -d myapp --service worker`.

`--service` requires `--deployment`. Setting `--service` without `-d` is rejected client-side. Service names follow the regex `^[a-z][a-z0-9-]{0,29}$`.

### secrets list

| Item | Details |
|------|---------|
| **Usage** | `dibbla secrets list [--deployment <alias> | -d <alias>] [--service <name> | -s <name>]` |
| **Flags** | `--deployment`, `-d` — list secrets for this deployment; omit for global |
| | `--service`, `-s` — scope to a single service (requires `-d`) |
| **Output** | Table: NAME, DEPLOYMENT, SERVICE, UPDATED. Service `(all)` means deployment-wide; deployment `(global)` means org-global. |

### secrets set

| Item | Details |
|------|---------|
| **Usage** | `dibbla secrets set <name> [value] [-d <alias>] [-s <service>]` |
| **Arguments** | `name` (required), `value` (optional — if omitted, read from stdin) |
| **Flags** | `--deployment`, `-d` — attach to deployment; omit for global |
| | `--service`, `-s` — scope to a single service (requires `-d`) |
| **Notes** | Per-service secrets stack on top of deployment-wide and global; the higher-precedence value wins inside the service container. |

### secrets get

| Item | Details |
|------|---------|
| **Usage** | `dibbla secrets get <name> [-d <alias>] [-s <service>]` |
| **Arguments** | `name` (required) |
| **Flags** | `--deployment`, `-d` — for deployment-scoped secret |
| | `--service`, `-s` — for per-service secret (requires `-d`) |
| **Output** | Secret value only (pipeline-friendly) |
| **Notes** | Returns the exact (deployment, service) row — there is no implicit fall-through. To inspect what a service container actually sees at runtime, exec into the pod or use `dibbla logs <alias> --service <svc>` after a redeploy. |

### secrets delete

| Item | Details |
|------|---------|
| **Usage** | `dibbla secrets delete <name> [-d <alias>] [-s <service>] [--yes | -y]` |
| **Arguments** | `name` (required) |
| **Flags** | `--deployment`, `-d` — for deployment-scoped secret |
| | `--service`, `-s` — for per-service secret (requires `-d`) |
| | `--yes`, `-y` — skip confirmation |

---

## admin

Platform-admin commands gated by `DIBBLA_ADMIN_TOKEN`. The user's normal API token is **not** used; admin endpoints require a separate static token configured by the platform operator. If the token isn't configured server-side, the endpoints don't exist (the CLI will see a 404).

### admin reconcile

Force one synchronous orphan-resource sweep on the deploy-api instance. The reconciler normally runs on a periodic schedule; this command runs one tick immediately and prints the names of the K8s objects it deleted (or would have, depending on operator config).

| Item | Details |
|------|---------|
| **Usage** | `DIBBLA_ADMIN_TOKEN=<tok> dibbla admin reconcile` |
| **Flags** | `--json` — emit the raw JSON sweep result instead of human text |
| **Auth** | Reads `DIBBLA_ADMIN_TOKEN` from env. The user's API token is NOT used. `DIBBLA_API_URL` (or default) selects the deploy-api instance. |
| **Output** | `deployments`, `services`, `ingresses` — counts plus the names of the swept objects. |
| **Errors** | Missing token → exit 1 with prompt. 401 → "unauthorized; check DIBBLA_ADMIN_TOKEN". 404 → "admin endpoints not enabled on this deploy-api instance". 503 → "reconciler not configured". |

**Examples:**
```bash
DIBBLA_ADMIN_TOKEN=$ADMIN_TOKEN dibbla admin reconcile
DIBBLA_ADMIN_TOKEN=$ADMIN_TOKEN dibbla admin reconcile --json | jq '.deployments'
```

---

## workflows

Alias: `wf`. All workflow commands support these persistent flags:

| Flag | Description |
|------|-------------|
| `--output`, `-o` | Output format: `yaml`, `json`, or `table` |
| `--quiet`, `-q` | Minimal output |
| `--verbose`, `-v` | Show HTTP request/response details |

> **What is a workflow?** A typed DAG of function calls authored in slim YAML. A *workflow* is a stable name; a *revision* is an immutable snapshot of its YAML. `HEAD` is the mutable working revision that every command below modifies unless `--revision` is passed. See [workflows.md](workflows.md) for the model, the YAML format, node-type roles, validator errors, and canonical shapes.

### workflows list

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows list` |
| **Output** | Table: NAME, LABEL, NODES, HAS_API (default); or JSON/YAML with `-o` |

### workflows get

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows get <name>` |
| **Arguments** | `name` (required) — workflow name |
| **Flags** | `--revision` — get a specific revision |
| **Output** | YAML (default) or JSON with `-o json` |

### workflows create

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows create --file <path>` or `-f <path>` |
| **Flags** | `--file`, `-f` (required) — workflow definition file (YAML or JSON) |

### workflows update

Full replacement of HEAD — not a merge. The CLI sends an `If-Match` header containing the current ETag (fetched automatically from the existing workflow), so concurrent edits return `412` instead of silently clobbering whoever wrote last.

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows update <name> --file <path>` |
| **Arguments** | `name` (required) — workflow to replace |
| **Flags** | `--file`, `-f` (required) — workflow definition file (YAML or JSON) |
|  | `--force` — override the optimistic-concurrency check; overwrite even if HEAD has moved since the CLI last read it. Skip the `If-Match` precondition. |
| **Errors** | `412 Precondition Failed` with a JSON body of shape `{"error":"…","current_etag":"…","received_etag":"…"}` when another writer modified HEAD between the CLI's pre-update `wf get` and the `PUT`. Re-fetch with `wf get`, re-apply your changes, and retry — or pass `--force` to overwrite. |

**Agent guidance:** prefer the pull-merge-retry path over `--force`. The 412 is the system telling you a teammate (or the browser editor) shipped a change you didn't see; overriding it with `--force` may delete their work. Use `--force` only for known-overwrite cases (e.g. a CI job re-applying a stable golden definition).

### workflows delete

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows delete <name>` |
| **Arguments** | `name` (required) |
| **Flags** | `--yes` — skip confirmation |

### workflows validate

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows validate --file <path>` or `-f <path>` |
| **Flags** | `--file`, `-f` (required) — workflow definition to validate (not saved) |
| **Behavior** | Pure validation — never persists. Safe to run repeatedly during authoring. Returns the list of validation rule violations (`UNSATISFIED_INPUT`, `UNKNOWN_FUNCTION`, etc. — see [workflows.md](workflows.md) §10). |

### workflows execute

Execute a workflow over HTTP. Synchronous by default — the call blocks until the workflow's `api_response` node fires (server-side timeout: 30 min). For long-running or fire-and-forget runs, use `--async` to detach, or `--follow` to detach and tail logs in one command.

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows execute <name>` |
| **Arguments** | `name` (required) — workflow to execute |
| **Flags** | `--data` — inline JSON data to send |
| | `-F`, `--file` — JSON data file (note: short flag is `-F`, not `-f`, to free `-f` for `--follow`) |
| | `--node` — target a specific API node ID (only required when the workflow has multiple `api` nodes) |
| | `--async` — fire-and-forget: return `response_metadata` immediately and let the run continue in background |
| | `-f`, `--follow` — implies `--async`. Tail live logs to stdout until the run completes, then print the api_response payload. Exits 0 on the server-emitted `run_completed` sentinel |
| **Body shape** | JSON object **keyed by the `inputs:` names declared on the target `api` node** (e.g. `{"question":"…"}` if the api node has `inputs: [question]`). |
| **Response shape (sync, default)** | JSON object keyed by the `inputs:` names declared on the linked `api_response` node, plus `response_metadata: {run, node, workflow, timestamp}`. |
| **Response shape (`--async`)** | `{"response_metadata": {"run":"…", "node":"…", "workflow":"…"}}` only — the run is still in flight. Pair with `wf logs <runId> --follow` and/or `wf runs output <runId>` to retrieve progress and final output. |
| **Response shape (`--follow`)** | NDJSON log lines streamed to stdout, then a final JSON object identical to the sync response once the run finishes. |

**Examples:**
```
dibbla wf execute weather --data '{"question":"Berlin?"}'
dibbla wf execute weather --data '{"question":"…"}' --async       # returns immediately
dibbla wf execute weather --data '{"question":"…"}' --follow      # tail + final output
dibbla wf execute weather --file payload.json --follow
```

**Agent guidance:** prefer `--follow` for interactive debugging — you get live operational logs and the final api_response payload in one command. Use `--async` when dispatching many runs to inspect later via `wf runs list` + `wf runs output`. The default sync mode is fine for short, deterministic workflows but blocks the agent's terminal until the api_response fires.

### workflows url

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows url <name>` |
| **Arguments** | `name` (required) |
| **Flags** | `--revision` — URL for a specific revision |
| **Output** | Plain URL (default); JSON/YAML with `-o` |

### workflows api-docs

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows api-docs <name>` |
| **Arguments** | `name` (required) |
| **Flags** | `--revision` — docs for a specific revision |
| **Output** | Human-readable endpoint docs (default); JSON/YAML with `-o` |

### workflows logs

Tail structured operational logs for a workflow run. Logs are emitted by both `workflow-server` (orchestration) and `go-toolserver` (function/agent execution) and tagged with `run`, `workflow`, `node`, `level`, and `src`. Wire format matches `dibbla logs` (NDJSON over chunked HTTP) so the same renderer is reused.

| Item | Details |
|------|---------|
| **Usage** | `dibbla workflows logs <runId>` |
| **Arguments** | `runId` (required) — run id from a previous `wf execute` response or `wf runs list` |
| **Flags** | `--since <duration>` — backfill window for persisted entries (default `15m`) |
| | `-f`, `--follow` — keep the connection open; live entries stream until the run completes (server emits a `run_completed` sentinel) or you Ctrl-C |
| | `-n`, `--tail <N>` — show only the last N persisted entries (instead of the `--since` window) |
| | `--level <debug\|info\|warn\|error>` — minimum level to print (default `info`) |
| | `--json` — emit raw NDJSON instead of the human format |
| | `--no-color` — disable color output |
| **Behavior** | Finished runs short-circuit to historic-only and exit immediately — no waiting on a stream that has nothing to deliver. Live runs follow until `run_completed`. |
| **Persistence model** | WARN/ERROR + the `run_completed` sentinel are persisted to the database. INFO/DEBUG are live-only (no DB row). Tailing a quiet finished run will show essentially just `run completed`. |
| **Errors** | 404 if the run is not in the caller's organisation. |

**Examples:**
```
dibbla wf logs 020b1341-…                     # historic backfill (mostly WARN/ERROR)
dibbla wf logs 020b1341-… --follow             # live tail until the run completes
dibbla wf logs 020b1341-… --level debug -f     # see everything, including INFO/DEBUG
dibbla wf logs 020b1341-… --json | jq '.line'  # NDJSON for scripting
```

**Agent guidance:** for an in-flight run, `--follow` is the equivalent of `dibbla logs -f` for deployed apps — it's the primary "what is the workflow doing right now" view. For a finished run, the most useful artefact is usually `wf runs output <runId>` (the api_response payload), not the logs — INFO/DEBUG aren't persisted, so a clean run's historic tail is essentially empty.

---

## runs

Inspect workflow runs independent of any specific workflow command — useful when you have a run id but don't want to look up the workflow first, or when listing recent runs across all workflows.

### runs list

| Item | Details |
|------|---------|
| **Usage** | `dibbla wf runs list` |
| **Flags** | `-w`, `--workflow <name>` — filter by workflow name (matches both `name` and the `name/HEAD` canonical form) |
| | `-n`, `--limit <N>` — max rows (default 50, server caps at 500) |
| **Output** | Table (`ID`, `WORKFLOW`, `STARTED`) by default; JSON or YAML with `-o`. |

**Examples:**
```
dibbla wf runs list                       # 50 most recent runs across all workflows
dibbla wf runs list -w chat_agent         # only chat_agent runs
dibbla wf runs list -n 200 -o json        # raw JSON, scriptable
```

### runs output

| Item | Details |
|------|---------|
| **Usage** | `dibbla wf runs output <runId>` |
| **Arguments** | `runId` (required) — id of a finished run |
| **Output** | JSON object: the api_response payload merged with `response_metadata`. Same shape as a synchronous `wf execute` response. |
| **Errors** | 404 if the run isn't in the caller's organisation, hasn't reached an api_response yet, or the workflow has no api_response node. |

**Examples:**
```
dibbla wf runs output 020b1341-…
dibbla wf runs output 020b1341-… -o yaml
```

**Agent guidance:** the canonical async loop is `wf execute --async` → capture the run id from `response_metadata.run` → `wf logs <runId> -f` for live progress → `wf runs output <runId>` for the final payload. When the user asks "what did this run actually return", this is the command — `wf logs` is operational, not product output.

---

## nodes

> All `nodes` / `edges` / `inputs` / `tools` commands patch the workflow's HEAD revision. They do **not** auto-snapshot — pair risky patch sequences with `dibbla revisions create <workflow>` before and after for safe rollback.

### nodes add

| Item | Details |
|------|---------|
| **Usage** | `dibbla nodes add <workflow>` |
| **Arguments** | `workflow` (required) — target workflow name |
| **Flags** | `--file`, `-f` — node definition file (YAML/JSON) |
| | `--inline` — inline node definition (JSON string) |
| **Rule** | Either `--file` or `--inline` is required |

### nodes remove

| Item | Details |
|------|---------|
| **Usage** | `dibbla nodes remove <workflow> <node_id>` |
| **Arguments** | `workflow` (required), `node_id` (required) |
| **Flags** | `--yes` — skip confirmation |

---

## edges

### edges add

| Item | Details |
|------|---------|
| **Usage** | `dibbla edges add <workflow> "<src.port -> tgt.port>"` |
| **Arguments** | `workflow` (required), edge spec (required) |

### edges remove

| Item | Details |
|------|---------|
| **Usage** | `dibbla edges remove <workflow> "<src.port -> tgt.port>"` |
| **Arguments** | `workflow` (required), edge spec (required) |

### edges list

| Item | Details |
|------|---------|
| **Usage** | `dibbla edges list <workflow>` |
| **Arguments** | `workflow` (required) |
| **Output** | Table (default); JSON/YAML with `-o` |

---

## inputs

### inputs set

| Item | Details |
|------|---------|
| **Usage** | `dibbla inputs set <workflow> <node> <input> <value>` |
| **Arguments** | `workflow`, `node`, `input`, `value` (all required) |
| **Flags** | `--null` — set value to null instead of string |

---

## tools

### tools add

| Item | Details |
|------|---------|
| **Usage** | `dibbla tools add <workflow> <agent> <tool>` |
| **Arguments** | `workflow`, `agent` (node ID), `tool` (all required) |

### tools remove

| Item | Details |
|------|---------|
| **Usage** | `dibbla tools remove <workflow> <agent> <tool>` |
| **Arguments** | `workflow`, `agent` (node ID), `tool` (all required) |

---

## revisions

Alias: `rev`.

### revisions list

| Item | Details |
|------|---------|
| **Usage** | `dibbla revisions list <workflow>` |
| **Arguments** | `workflow` (required) |
| **Output** | Table: ID, TIMESTAMP, LABEL (default); JSON/YAML with `-o` |

### revisions create

| Item | Details |
|------|---------|
| **Usage** | `dibbla revisions create <workflow>` |
| **Arguments** | `workflow` (required) |
| **Output** | Revision ID (with `-q` prints only the ID) |

### revisions restore

| Item | Details |
|------|---------|
| **Usage** | `dibbla revisions restore <workflow> <revision_id>` |
| **Arguments** | `workflow` (required), `revision_id` (required) |
| **Behavior** | Makes `<revision_id>` the new HEAD by overwriting the current HEAD. **Not** a checkout — once HEAD is overwritten, the previous HEAD is lost unless it had been snapshotted. Always run `revisions create` first if the current HEAD is worth keeping. |

---

## functions

Alias: `fn`.

### functions list

| Item | Details |
|------|---------|
| **Usage** | `dibbla functions list` |
| **Flags** | `--server` — filter by server name |
| | `--tag` — filter by tag |
| **Output** | Table: NAME, SERVER, DESCRIPTION, TOOLS (default); JSON/YAML with `-o` |

### functions get

| Item | Details |
|------|---------|
| **Usage** | `dibbla functions get <server> <name>` |
| **Arguments** | `server` (required), `name` (required) |
| **Output** | YAML (default) or JSON with `-o json`. The `inputs` and `outputs` blocks include each field's actual Go-reflected type: `boolean`, `integer`, `float`, or `string`. |

**Agent guidance:** since the field-types fix, `fn get` is the **trusted source of truth** for input/output types. Older cached output (or pre-fix workflow YAML files saved to disk) may report everything as `string`; treat post-fix `fn get` as authoritative, and reach for the function source at `go-toolserver/functions/<name>/function.go` if `fn get` and a workflow's hardcoded type still disagree. Mismatched types fail at runtime with `cannot unmarshal X into Go struct field Inputs.Y of type Z`.

---

---

## Summary table

| Area | Command | Purpose |
|------|---------|---------|
| Auth | `dibbla login [api_url]` | Interactive browser/paste login (real TTY) |
| Auth | `dibbla login --browser` | Non-TTY browser OAuth (Claude Code, agent shells) |
| Auth | `dibbla login --api-key <token>` | Headless token login (CI, scripted) |
| Auth | `dibbla logout` | Clear stored credentials |
| Run | `dibbla run [path\|url]` | Execute a dibbla-task.yaml pipeline locally |
| Run | `dibbla run --preview <arg>` | Parse + print execution plan (no execution) |
| Template | `dibbla template list` | List available templates from the hosted manifest |
| Template | `dibbla template install <id> [<dir>]` | Materialize a template into a directory and run its bootstrap |
| Feedback | `dibbla feedback <message>` | Send feedback |
| Feedback | `dibbla feedback list` | List feedback |
| Feedback | `dibbla feedback delete <id>` | Delete feedback |
| Deploy | `dibbla deploy [path]` | Deploy app from directory |
| Apps | `dibbla apps list` | List deployments |
| Apps | `dibbla apps update <alias> ...` | Update env, replicas, cpu, memory, port, login guard |
| Apps | `dibbla apps delete <alias>` | Delete deployment |
| Db | `dibbla db list [-q]` | List databases |
| Db | `dibbla db create [name]` | Create database |
| Db | `dibbla db delete <name>` | Delete database |
| Db | `dibbla db dump <name> [-o file]` | Download dump |
| Db | `dibbla db restore <name> -f <file>` | Restore from dump |
| Db | `dibbla db connect <name> [-q]` | Print connection string |
| Secrets | `dibbla secrets list [-d alias]` | List global or app secrets |
| Secrets | `dibbla secrets set <name> [value] [-d alias]` | Create/update secret |
| Secrets | `dibbla secrets get <name> [-d alias]` | Print secret value |
| Secrets | `dibbla secrets delete <name> [-d alias]` | Delete secret |
| Workflows | `dibbla workflows list` | List all workflows |
| Workflows | `dibbla workflows get <name>` | Get workflow definition |
| Workflows | `dibbla workflows create -f <file>` | Create workflow from file |
| Workflows | `dibbla workflows update <name> -f <file>` | Replace workflow definition |
| Workflows | `dibbla workflows delete <name>` | Delete workflow |
| Workflows | `dibbla workflows validate -f <file>` | Validate without saving |
| Workflows | `dibbla workflows execute <name>` | Execute workflow |
| Workflows | `dibbla workflows url <name>` | Get UI URL |
| Workflows | `dibbla workflows api-docs <name>` | Show API endpoint docs |
| Nodes | `dibbla nodes add <wf> -f <file>` | Add node to workflow |
| Nodes | `dibbla nodes remove <wf> <id>` | Remove node |
| Edges | `dibbla edges add <wf> "<edge>"` | Add edge |
| Edges | `dibbla edges remove <wf> "<edge>"` | Remove edge |
| Edges | `dibbla edges list <wf>` | List edges |
| Inputs | `dibbla inputs set <wf> <node> <input> <val>` | Set node input |
| Tools | `dibbla tools add <wf> <agent> <tool>` | Add tool to agent |
| Tools | `dibbla tools remove <wf> <agent> <tool>` | Remove tool from agent |
| Revisions | `dibbla revisions list <wf>` | List revisions |
| Revisions | `dibbla revisions create <wf>` | Create snapshot |
| Revisions | `dibbla revisions restore <wf> <id>` | Restore revision |
| Functions | `dibbla functions list` | List available functions |
| Functions | `dibbla functions get <server> <name>` | Get function details |
