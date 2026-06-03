# Dibbla CLI — Pre-deploy guardrails

Before calling `dibbla deploy`, you **MUST** complete all seven checks below and present findings to the user. **Never deploy autonomously** — always wait for explicit user confirmation.

Checks 1–4 and Check 7 are mandatory for every deploy. Check 5 only fires when running task files from URLs (`dibbla run <url>` / `dibbla template install`). Check 6 only fires when a `dibbla.yaml` is present at the deploy root.

> **Enforced by the CLI.** `dibbla deploy` refuses to upload when `REVIEW.md` is missing at the deploy root, or when no user handbook (`docs/index.md` or `APP.md`) is present. The only way past the gate is `--skip-review`, which is reserved for humans making one-line fixes — agents must run this checklist and write `REVIEW.md` (see Step 3.5) rather than passing the flag.

---

## Severity levels

- **BLOCKER** — Must fix before deploying. Do NOT call `dibbla deploy`.
- **WARNING** — Should fix. Present to the user and deploy only if they explicitly confirm.

---

## Check 1: Security (OWASP Top 10)

Scan all application source files for:

| What to check | Severity | Examples |
|----------------|----------|----------|
| Hardcoded secrets (API keys, passwords, tokens, connection strings) | BLOCKER | `const apiKey = "sk-..."`, `password = "admin123"`, strings matching `sk-`, `ak_`, `ghp_`, `password\s*=\s*["'][^"']+` |
| SQL injection (string concatenation/interpolation in queries) | BLOCKER | `` `SELECT * FROM users WHERE id = ${id}` ``, `"SELECT * FROM users WHERE id = " + id` |
| Command injection (unsanitized input in shell commands) | BLOCKER | `exec("rm " + userInput)`, `os.system(f"ls {path}")`, `child_process.exec(userInput)` |
| XSS (unsanitized user input rendered in HTML) | BLOCKER | `innerHTML = userInput`, `dangerouslySetInnerHTML` without sanitization |
| `.env` files present in the deploy directory | BLOCKER | `.env`, `.env.local` not in `.gitignore` / `.dockerignore` |
| Missing CSRF protection on state-changing endpoints | WARNING | POST/PUT/DELETE routes with no CSRF token or SameSite cookie |
| Insecure deserialization / eval | WARNING | `eval()` on user input, `pickle.loads()` on untrusted data, `yaml.load()` without `SafeLoader` |
| Missing input validation on API endpoints | WARNING | No request body validation, no type checking on route params |
| Sensitive data in logs | WARNING | Logging passwords, tokens, or PII to stdout/console |
| Missing security headers | WARNING | No `helmet()` (Node), no CORS configuration, no `Content-Security-Policy` |

---

## Check 2: Database usage

Scan all database-related code for:

| What to check | Severity | Examples |
|----------------|----------|----------|
| N+1 queries (query inside a loop) | BLOCKER | `for user in users: db.query("SELECT * FROM orders WHERE user_id = ...")` |
| Unbounded queries (SELECT without LIMIT) | WARNING | `SELECT * FROM large_table` with no `LIMIT` or pagination |
| Missing connection pooling | WARNING | Creating a new DB connection per request instead of using a pool |
| Missing error handling on DB operations | WARNING | No try/catch around queries, no transaction rollback on failure |
| Schema migrations without safeguards | WARNING | `DROP TABLE`, `DROP COLUMN` without backup or confirmation |

---

## Check 3: REST / API call patterns

Scan all outbound HTTP/API call code for:

| What to check | Severity | Examples |
|----------------|----------|----------|
| No timeout on outbound HTTP calls | BLOCKER | `fetch(url)` or `requests.get(url)` with no timeout option |
| Missing retry with exponential backoff | WARNING | Single HTTP call with no retry logic for transient failures (5xx, network errors) |
| Excessive polling (interval < 5 seconds) | WARNING | `setInterval(poll, 1000)`, tight polling loops |
| No error handling on API responses | WARNING | Not checking HTTP status codes, not handling network errors |
| Hardcoded external URLs | WARNING | Third-party API URLs inline in source instead of env vars / config |
| Missing rate limiting on inbound endpoints | WARNING | Public-facing API routes with no rate limiting middleware |

---

## Check 4: External system write safety

Scan code that writes to external systems (APIs, queues, email, SMS, webhooks, third-party services):

| What to check | Severity | Examples |
|----------------|----------|----------|
| Unbounded writes in a loop (no batching) | BLOCKER | `for item in items: api.post("/send", item)` — should batch or throttle |
| No rate limiting on outgoing calls | WARNING | Sending hundreds of emails/SMS/webhooks with no throttle or delay |
| Missing idempotency on write operations | WARNING | No idempotency key on payment, order creation, or webhook delivery calls |
| Fire-and-forget writes (no error handling) | WARNING | Write calls with no error capture, no retry, no dead-letter handling |
| Missing queue for bulk operations | WARNING | Synchronously sending thousands of notifications instead of using a job queue |

---

## Check 5: Running task files from URLs

When the user asks you to run a `dibbla-task.yaml` from a URL (via `dibbla run <url>` or `dibbla template install <id>`), apply these checks before executing:

| What to check | Severity | Examples |
|----------------|----------|----------|
| Source trust | WARNING | `dibbla run <https-url>` fetches and executes shell commands from the network. Treat it like `curl … \| bash`. Only run yamls from sources the user trusts (e.g. `github.com/dibbla-agents/*` bootstraps or yamls authored by the user themselves). If the URL is from an unknown third party, warn the user and offer `dibbla run --preview <url>` first to inspect the plan. |
| Work-dir side effects | INFO | URL-fetched yamls execute with the user's invocation CWD as the work dir. Bootstrap yamls typically `git clone` into that directory. If the user's CWD is not empty (e.g. has existing files), make sure the clone step won't collide — prefer `mkdir fresh-dir && cd fresh-dir` before running. |
| Self-install / self-update steps | INFO | Some template task files include steps like `brew upgrade dibbla` or `curl install.sh \| sh`. These replace the on-disk dibbla binary while dibbla itself is running. This is benign on macOS/Linux (the running process keeps the old mmap) but users won't see the new version until their next re-invocation. Mention this if it surfaces in the output. |

---

## Check 6: Multi-service manifest safety

Run when a `dibbla.yaml` (or `dibbla.yml`) is present at the deploy root. Skip otherwise.

| What to check | Severity | Examples |
|----------------|----------|----------|
| Every `public: true` service has a `port:` | BLOCKER | A service `public: true` without `port:` fails the deploy with `PUBLIC_MISSING_PORT`. |
| `depends_on:` references real services in the manifest | BLOCKER | `depends_on: [redis]` when no `redis` service exists. |
| No `depends_on:` cycle | BLOCKER | `web → worker → web`. The validator detects cycles. |
| `expose_to:` references real services in the manifest | BLOCKER | `expose_to: [api]` when no `api` service exists. |
| Resource sums fit org quota (8 services, 20 replicas, 8 CPU, 16Gi mem, 50Gi PVC) | BLOCKER | A `replicas: 12` per service that pushes the total over 20. Surfaces as `QUOTA_EXCEEDED`. |
| Image refs include a tag | BLOCKER | `image: redis` (rejected); use `image: redis:7`. |
| No reserved service names (`proxy`, `auth`, `system`, `dibbla`, `kube-*`) | BLOCKER | `services: { proxy: ... }`. |
| Build context exists in the archive | BLOCKER | `build: ./web` when no `./web` directory exists. |
| Init containers exit cleanly | WARNING | An init that runs forever (e.g. `command: [sh, -c, "while true; do sleep 60; done"]`) blocks the rollout and times out the deploy. Inits are for migrations, schema sync, asset hydration — not long-running processes. |
| Healthcheck `failure_threshold` ≥ 3 in production | WARNING | `failure_threshold: 1` will kill the pod on a single transient failure. |
| Build-time secrets referenced in `build.secrets:` exist as dibbla secrets | BLOCKER | `secrets: [{id: npm, source: NPM_TOKEN}]` requires `dibbla secrets list -d <alias>` to show `NPM_TOKEN`. |
| Multiple `public: true` services | INFO | Each gets `<alias>-<service>.<base-domain>`; the lex-first one also gets the bare `<alias>.<base-domain>`. Confirm the user knows the URL shape and which service owns the bare alias. |
| Per-service auth missing on a sensitive public service | WARNING | If a public service name suggests an admin/internal UI (`pgadmin`, `adminer`, `mailhog`, `bull`, `redis-commander`, `grafana`, `prometheus`, or names containing `admin` / `internal` / `debug` / `tools`), require explicit user confirmation that **either** the service has `auth.require_login: true` set, **or** it's gated by `profiles: [dev]`. Shipping an admin UI publicly without auth is a top OWASP-class mistake. |
| Hostname collision with existing alias | BLOCKER | Multi-public deploys produce hyphenated hostnames `<alias>-<service>`. If any existing alias in the org matches one of those strings, the deploy fails with `ALIAS_HOSTNAME_COLLISION`. Surface to the user before deploy by checking `dibbla apps list` for collisions, especially on aliases that already contain hyphens. |

When a `dibbla.yaml` is present, run `dibbla manifest validate` before the deploy and confirm the result with the user. For env-aware / quota / cross-service-reference checks, also run `dibbla preview --target-env <env>` — the local validator can't see env-aware values or org quotas.

---

## Check 7: User handbook (end-user documentation)

Mandatory for every deploy. The platform renders a user-facing handbook inside `auth.dibbla.net` under "My Apps → {alias}" — this is the only documentation surface end users see. See [user-docs.md](user-docs.md) for the full audience guidance, file conventions, tone rules, and paste-ready templates.

| What to check | Severity | Examples |
|----------------|----------|----------|
| At least one of `docs/index.md` or `APP.md` exists at the project root | BLOCKER | Neither file present at the deploy root. The platform will accept the deploy, but the user-facing handbook will be empty — refuse to deploy until the user agrees to ship documentation. |
| When `docs/` exists, `docs/index.md` is present | BLOCKER | A `docs/` folder with no `index.md` — the deploy will fail with a clear error. Generate the landing page from the template in [user-docs.md](user-docs.md). |
| The landing page (`docs/index.md` or `APP.md`) has a `subtitle:` frontmatter, and it is end-user-facing | BLOCKER | Missing frontmatter, or `subtitle:` absent, or the value still contains placeholders (`TBD`, `TODO`, `<one short…>`, `{{app_name}}`), or it leaks technical detail (framework names, "deployed via X", "Node.js", env-var names). The card on the My Apps grid relies on this single line — without it, end users see "Deployed application" as the blurb. Write a real subtitle following the rules in [user-docs.md](user-docs.md). |
| Subtitle is ≤ 140 bytes (target ≤ 70 chars), one sentence, plain text | BLOCKER | The bundler rejects subtitles over 140 bytes. The auth-ui My Apps card is ~180px wide and CSS-clamps to two lines, so anything past ~70 English chars gets visually clipped. Trim until it fits one tight sentence — start with a verb ("Track…", "Send…", "Manage…"), drop filler like "This is an app for…". No emoji, no markdown, no multi-line. |
| Handbook content is for the **end user**, not for developers | BLOCKER | `docs/` or `APP.md` describes the dev stack ("Built with Vite + React + TailwindCSS"), env vars (`DATABASE_URL`), Docker/Dockerfile, deploy commands, source paths, or framework names. Strip and rewrite for the end user (see [user-docs.md](user-docs.md) anti-examples table). |
| Per-page size ≤ 200 KiB; total bundle ≤ 800 KiB | BLOCKER | The deploy fails with `Invalid user docs:` or `User docs bundle too large:` — split into smaller pages or move large assets out of `docs/`. |
| `_nav.yaml` references valid page slugs (if present) | BLOCKER | `_nav.yaml` mentions a slug for which no `.md` exists — the deploy fails with `references missing page`. |
| Handbook covers Welcome, Getting Started, and FAQ at minimum | WARNING | A handbook with only one page and no "How do I…" guidance leaves users stuck — propose adding the three core sections from [user-docs.md](user-docs.md) before deploy. |
| No placeholder text (`TBD`, `TODO`, `lorem ipsum`, `{{app_name}}` placeholders) remains | WARNING | The templates use `{{app_name}}` / `{{org_name}}` — these MUST be replaced with real values before deploy. |
| Cross-page relative links resolve to real pages | WARNING | `[Foo](./foo.md)` where `docs/foo.md` doesn't exist — broken links don't block the deploy but show a "page not found" card to the user. |

**Workflow when handbook is missing:**

1. Tell the user the handbook is missing.
2. Offer to generate a starter handbook from the templates in [user-docs.md](user-docs.md) — `docs/index.md` + `docs/getting-started.md` + `docs/faq.md` at minimum.
3. Fill in the templates with content specific to the app (do not invent features that don't exist; ask the user what the app does and what the user-facing flows are).
4. **Write a real `subtitle:` frontmatter on `docs/index.md`** — one short user-facing sentence, ≤ 70 chars (hard cap 140 bytes), starts with a verb, sentence case, ends with a period. This is what shows on the card.
5. Show the user the generated files and wait for explicit confirmation.
6. Only then deploy.

**Workflow when content is technical, not user-facing:**

1. Show the user the offending lines (with file path + line number).
2. Propose a rewrite that strips dev-stack info and reframes the content for an end user.
3. Apply the rewrite and re-run Check 7.

---

## Interactive workflow

### Step 1: Run all applicable checks

Review the application source code against every applicable check above (1–4 and 7 always; 5 if running task files from URLs; 6 if a `dibbla.yaml` is at the deploy root). Note each finding with its file path and line number.

### Step 2: Present the report

Show the user a guardrails report in this format:

```
## Pre-deploy guardrails report

- [x] Security (OWASP Top 10): OK
- [ ] Database usage: 1 BLOCKER, 2 warnings
  - BLOCKER: N+1 query in `src/routes/orders.js:42` — query inside forEach loop
  - WARNING: Unbounded SELECT in `src/models/users.js:18` — add LIMIT or pagination
  - WARNING: No connection pooling — consider using a connection pool
- [x] REST/API calls: 1 warning
  - WARNING: No timeout on fetch in `src/services/payment.js:23` — add a timeout
- [x] External writes: OK
- [ ] User handbook: 1 BLOCKER
  - BLOCKER: No `docs/index.md` or `APP.md` at project root — generate from templates in user-docs.md.

**Result: BLOCKED** — 2 blockers must be fixed before deploying.
```

### Step 3: Wait for user confirmation

- **If BLOCKERs found:** Tell the user what needs fixing and offer to fix it. Wait for their confirmation before making changes. After fixing, re-run the checks and present an updated report. Do NOT deploy until all blockers are resolved and the user confirms.
- **If only WARNINGs:** Ask the user: *"There are N warnings. Should I fix these before deploying, or proceed as-is?"*
- **If all clear:** Ask the user: *"All guardrails checks passed. Ready to deploy?"*

### Step 3.5: Write REVIEW.md

After completing the guardrails review and before deploying, write a `REVIEW.md` file in the project root directory. This file is read by the platform and displayed as a review status indicator in the deployments dashboard.

**Format:**

```markdown
---
Review-status: Ok | Warnings | Critical
One-Sentence-Summary: "<brief summary of findings>"
---

<full guardrails report from Step 2>
```

**Status mapping:**
- `Ok` — all four checks passed with no blockers or warnings
- `Warnings` — no blockers found, but warnings are present (user chose to proceed)
- `Critical` — blockers were found and fixed before deploying

Always write this file, even when all checks pass. The platform shows a red indicator when REVIEW.md is missing, **and `dibbla deploy` will refuse to upload without it.**

### Step 4: Deploy only after confirmation

Only call `dibbla deploy` after the user has explicitly confirmed.
