# Dibbla CLI — User handbook (end-user docs)

Every deployable app **must** ship a user handbook. The platform renders it inside `auth.dibbla.net` under "My Apps → {alias}" — a sidebar of pages on the left, the rendered markdown on the right. This is the only documentation surface end users see. If an app ships without it, the deploy guardrails block.

This file tells you **what to write, where to write it, and what NEVER to write**.

---

## Audience

Write for the **end user of the deployed app**. Concretely, that's the human who clicks the app card in `auth.dibbla.net` and starts using the app. They:

- Don't know what language the app is written in. They don't care.
- Don't know what a container is, what a Dockerfile is, what Postgres is.
- Want to learn what the app does, how to do the most common tasks, and what to do when something goes wrong.

**Footgun:** It is tempting to write developer documentation here because that's the easiest thing for an AI agent to generate. Don't. If the content would only be useful to the person who *wrote* the app, it belongs in `README.md` (which is fine to also have), not in `docs/`.

**Audiences that should NEVER read this file:**
- Developers maintaining the app.
- Operators / SREs.
- AI agents working on the codebase.

---

## The `subtitle` frontmatter (mandatory)

Every handbook **must** declare a `subtitle:` in YAML frontmatter on its landing page (`docs/index.md` or `APP.md`). This single line is what shows under the app name in the "My Apps" grid — it is the only summary text most users will ever read. The bundler reads it from the file; if you omit it, the card falls back to a generic "Deployed application" placeholder.

```markdown
---
subtitle: Track invoices and get paid faster.
---

# Welcome to Acme

…rest of the page…
```

Rules:
- **Audience identical to the rest of the handbook.** Write for the end user, in the user's voice. Don't put the stack in the subtitle.
- **One short sentence. Aim for ≤ 70 characters; the hard cap is 140 bytes** (the bundler rejects longer values). The auth-ui My Apps card is ~180px wide and clamps to two lines, so anything past ~70 chars gets visually clipped on the card itself. Don't fight the layout — write tight.
- **Lead with the user's benefit, not the implementation.** "Send invoices to customers." beats "An invoicing app." beats "A Node.js + Postgres invoicing service."
- **Sentence case, ends with a period.** Plain text only — no markdown, no emoji.
- **No filler.** Skip "This is an app for…", "An app that helps you…", "A tool for…". Start with the verb the user performs: "Track…", "Schedule…", "Manage…", "Find…".

**Anti-examples** (every one of these has shipped — none belong in a subtitle):

| ❌ Wrong | ✅ Right |
|---|---|
| `subtitle: A Next.js app for managing tasks.` | `subtitle: Capture and finish daily tasks.` |
| `subtitle: TODO — fill this in` | *(deploy is blocked until you fill it in)* |
| `subtitle: Deployed via Dibbla` | `subtitle: <whatever the user actually does with it>` |
| `subtitle: This app helps users to manage their team's daily standup notes and share them with the rest of the company.` *(too long — clips on the card)* | `subtitle: Share your team's standup notes.` |
| `subtitle: This app helps users to manage…` | `subtitle: Manage your team's standup notes.` |

The subtitle is also returned to the auth-ui docs viewer and rendered under the app name on the handbook header, so it's always visible — both on the My Apps grid and inside the handbook.

---

## File layout

Two conventions are supported, checked in this order:

### 1. `docs/` folder (preferred — use this whenever the app has more than ~5 minutes of explanation)

```
myapp/
├── dibbla.yaml
└── docs/
    ├── index.md         # required — the landing page
    ├── _nav.yaml        # optional — explicit sidebar order
    ├── getting-started.md
    ├── features/
    │   ├── billing.md
    │   └── reports.md
    └── faq.md
```

Rules:
- `docs/index.md` is **required** when `docs/` exists. It is the page users land on.
- Sub-folders become collapsible groups in the sidebar. The group title is the folder name, humanised (`features` → "Features").
- Page titles come from the first H1 (`# Page Title`) in each file. Fall back to the humanised filename when there's no H1 (so `getting-started.md` → "Getting Started").
- Files starting with `.` or `_` are skipped (so `_nav.yaml`, `_drafts/`, `.gitkeep` are safe).
- Per-page cap: 200 KiB. Total bundle cap: 800 KiB. If you blow either, the deploy fails with a clear error message — split the content into more pages, or move large assets (screenshots, embedded videos) out of `docs/`.

### 2. `APP.md` at the project root (single-file fallback)

```
myapp/
├── dibbla.yaml
└── APP.md
```

Use this only for very small apps where the entire handbook is one short page. The platform synthesises a one-page bundle with `APP.md` as the landing page. The sidebar will have a single "Home" entry — no navigation tree.

If you create a `docs/` folder, the platform ignores `APP.md`. Don't ship both.

### `_nav.yaml` — explicit sidebar ordering

By default the sidebar is alphabetical, with `index.md` first in each folder. If you want a specific order or a group label different from the folder name, drop a `docs/_nav.yaml`:

```yaml
- page: index
- page: getting-started
  title: Quick Start            # overrides the page's own H1 for the sidebar
- group: Features
  pages:
    - features/billing
    - features/reports
- page: faq
```

`page:` references the slug (filename without `.md`, including folder prefix). `group:` makes a collapsible group with the listed `pages:` as children. Referencing a slug that doesn't exist fails the deploy — use `dibbla preview` or just deploy to see the error.

---

## What every handbook needs

The five sections below are the minimum. Use them as headings or as separate pages — either is fine.

1. **Welcome / What is this app?** — One paragraph. What problem does it solve? Who is it for? What can the user do after one minute of reading?
2. **Getting started** — The single most common path through the app, in five steps or fewer. "Click X, then Y, you'll see Z." Don't make the user piece together how things connect.
3. **Feature walkthroughs** — One short page per feature. Lead with the user's goal ("Send an invoice"), then the steps, then a one-line note on common variations.
4. **FAQ** — The five questions a user will actually ask. "How do I undo …", "Why is my … missing", "What happens when I …". Write them in the user's voice.
5. **Troubleshooting (optional)** — If the app has known rough edges, name them and tell the user what to do.

Keep total length **short**. A user handbook isn't a manual; it's a friendly tour. If a page exceeds ~400 words it probably wants splitting.

---

## Tone rules

- **Second person, present tense.** "You'll see a list of invoices." Not "The user will see a list of invoices." Not "Invoices will be shown."
- **One idea per paragraph.** Two or three sentences max.
- **Lead with the goal, not the mechanism.** "**To send an invoice:** click *Invoices*, then *New*…" — never "*Invoices* lives in the top nav and contains a *New* button."
- **No jargon.** If a word would baffle a non-developer (`endpoint`, `API`, `JSON`, `cache`, `container`, `env var`, `OAuth scope`, `webhook`), rephrase or omit. The exception is words the *app itself* uses in its UI — those are fine because the user sees them too.
- **Concrete > vague.** "Enter your email" beats "Provide your credentials."
- **No emojis** unless the user explicitly asks.

### Anti-examples (these have all shipped — none belong in user docs)

| ❌ Wrong (technical) | ✅ Right (user-facing) |
|---|---|
| "This app uses Postgres 16 with a connection pool of 20." | *(delete — the user doesn't care)* |
| "Set the `DATABASE_URL` env var before deploying." | *(delete — env vars are not the user's problem)* |
| "Run `npm run dev` to start the local server." | *(delete — that's dev docs)* |
| "Built with React, Vite, and TailwindCSS." | *(delete)* |
| "Click the button to POST `/api/orders` with the form data." | "Click *Place order*. Your order shows up under *My Orders*." |
| "Configured via OAuth scope `drive.readonly`." | "Sign in with Google. We'll ask permission to read your Drive files — nothing else." |

If you find yourself writing a sentence with backticks, ask: would the user ever type or see that string? If no, delete it.

---

## Cross-linking syntax

Links between pages use ordinary relative markdown links. The viewer rewrites them to navigate within the handbook (no full page reload).

- **To another page**: `[Getting started](./getting-started.md)`
- **To a page in a subfolder**: `[Billing](./features/billing.md)`
- **From a subfolder to a root page**: `[FAQ](../faq.md)`
- **To an anchor on the same page**: `[the limits section](#limits)` — anchors are auto-generated from heading text (slugified): `## Sending limits` → `#sending-limits`.
- **To an anchor on another page**: `[delete an invoice](./features/billing.md#deleting)`.
- **External link**: `[Stripe docs](https://stripe.com/docs)` — opens in a new tab automatically.

The viewer never reloads the page or leaves `auth.dibbla.net`, so make sure your links resolve. The bundler currently does *not* hard-fail on broken internal links (so a typo in a relative link won't block the deploy), but the user will see a friendly "page not found" card if they click one.

---

## Templates (paste-ready)

Each template uses `{{app_name}}` and `{{org_name}}` as placeholders — fill them in before deploy. Keep the structure; rewrite the content for the specific app.

### `docs/index.md`

```markdown
---
subtitle: <one short user-facing sentence starting with a verb, ≤ 70 chars, ends with a period.>
---

# Welcome to {{app_name}}

{{app_name}} helps you _<one-sentence description of what the app does for the user>_.

This handbook walks you through everything the app can do. If you're new, start with [Getting started](./getting-started.md). If you have a specific question, jump to the [FAQ](./faq.md).

## What's inside

- **[Getting started](./getting-started.md)** — your first five minutes with {{app_name}}.
- **[Features](./features/billing.md)** — what each part of the app does.
- **[FAQ](./faq.md)** — the questions other users have asked.
```

### `docs/getting-started.md`

```markdown
# Getting started

This page gets you from "I just opened the app" to "I know what to do next" in about two minutes.

## Step 1 — Sign in

Open {{app_name}} from your apps page in {{org_name}}. Sign in with your Google account if you haven't already.

## Step 2 — _<replace with the next concrete user action>_

_<one short paragraph plus a screenshot or sentence describing what they'll see>_

## Step 3 — _<replace with the third action>_

_<…>_

## What's next

- Learn about [_<a feature they'll want next>_](./features/_feature_.md).
- Skim the [FAQ](./faq.md) for common questions.
```

### `docs/faq.md`

```markdown
# FAQ

## How do I _<common task>_?

_<answer in two sentences max — link to the deeper page if needed>_

## Why can't I _<thing the user expected to be able to do>_?

_<short honest answer — link to a feature page if the answer is "this feature works differently than you think">_

## What happens if _<edge case>_?

_<short answer>_
```

### `docs/_nav.yaml`

```yaml
- page: index
- page: getting-started
- group: Features
  pages:
    - features/billing
    - features/reports
- page: faq
```

---

## What NEVER to write here

This is a deny-list. If your handbook contains any of these, delete or move them — they belong in source comments, the project README, or platform configuration, not in user docs.

- **Dev stack details** — framework names, language versions, library lists, "built with X".
- **Deploy / infra commands** — `dibbla deploy …`, `docker build …`, `kubectl …`, anything starting with a shell prompt.
- **Environment variables** — `DATABASE_URL`, `JWT_SECRET`, `OPENAI_API_KEY`, anything `UPPER_SNAKE_CASE`. The user doesn't have these and doesn't need them.
- **Source code paths** — `src/routes/orders.ts`, `internal/handlers/billing.go`. Users have no source tree.
- **Internal team info** — author email, Slack channel, on-call rotation, ticket links.
- **Architecture diagrams** — service maps, data-flow diagrams, ER diagrams. If a *user* needs one of these to understand the app, the app's UX has failed.
- **API reference** — `POST /api/v1/orders` with JSON schemas. If the app has a public developer-facing API, document that *separately* — not in the user handbook.
- **TODO / WIP markers** — if a section isn't ready, omit it entirely; ship a smaller handbook rather than one with `(TBD)` stubs.

---

## Workflow when scaffolding a new app

When you create a new project with `dibbla template install` or by hand:

1. Create `docs/` with at least `docs/index.md`, `docs/getting-started.md`, and `docs/faq.md` using the templates above.
2. **Write the `subtitle:` frontmatter on `docs/index.md` first** — it forces you to articulate the app's one-line value proposition before you start drafting longer prose. If you can't write the subtitle, you don't yet know what the app does for the user.
3. Fill in `{{app_name}}` placeholders and replace the example sections with content specific to the app you're building.
4. Confirm content with the user before deploying — never invent feature documentation for features that don't exist yet.
5. Deploy. The platform's pre-deploy guardrail (Check 7 in [guardrails.md](guardrails.md)) verifies the handbook is present **and that the subtitle is set**.

When you redeploy an existing app:

1. If you added a feature, add a corresponding page or section to the handbook **in the same change**. Shipping the feature without doc updates means the next user who opens the handbook sees stale information.
2. If you removed or renamed a feature, remove or rename the page. Broken internal links don't fail the deploy but they reflect badly on the app.

---

## How docs render in the console

The auth-ui portal (`auth.dibbla.net`) fetches the bundle via `GET /deployments/{alias}/docs` and renders it with `react-markdown` + GitHub-flavoured-markdown + heading anchors. Concretely:

- **Tables, task lists, strikethrough, fenced code blocks** — all work.
- **Headings get auto-generated IDs** (slugified) so `[#section]` anchor links scroll to them.
- **Relative `.md` links** are intercepted and turned into in-app navigation (no page reload).
- **External links** (`http://`, `https://`) open in a new tab.
- **HTML in markdown** is *not* rendered as HTML by default — write plain markdown.

The viewer has no editing UI. To change the docs, edit source files and redeploy. There is no `dibbla docs push` shortcut today.
