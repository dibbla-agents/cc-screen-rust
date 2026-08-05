# cc-screen-site

The cc-screen landing page at **https://ccscreen.dev** — the acquisition funnel
for the hosted SaaS at **https://app.ccscreen.dev**, with a preserved self-host
track (proposal 0054).

- **`web/`** — the source: a **Vite + React + TypeScript + Tailwind** app.
  External URLs (app, site, docs, GitHub) live in one place:
  `web/src/urls.ts`.
- **Design-language contract (proposal 0057).** The site deliberately mirrors
  the product app's own look so signing up feels like walking deeper into the
  same room. `web/src/index.css`'s `@theme` tokens (`bar`/`panel`/`edge`/
  `accent`/`amber`, the slate text ramp, the per-tool hues) mirror
  `frontend/tailwind.config.js` — **if the app's tokens move, move these with
  them.** The only green left is `--color-live` (a running session). Motion
  (the live hero panel, the ambient backdrop, the one `Reveal` scroll system)
  reuses the app's own keyframes and is fully gated behind
  `prefers-reduced-motion`.
- **`../docs`** — the build output, committed so **GitHub Pages** can serve it
  (from `/docs` on `main`) at the custom domain `ccscreen.dev`
  (`web/public/CNAME` → `docs/CNAME`). The `docs/docs/` subtree is **owned by
  proposal 0055** (the end-user docs) — the site build sets
  `emptyOutDir: false` and prunes only `docs/assets`, so a rebuild never
  deletes it.
- **this crate** — a tiny Rust (axum + tower-http) static-file server for the
  legacy **Dibbla** `cc-screen` app. With `SITE_CANONICAL` set (the deploy
  default: `https://ccscreen.dev`) it 301-redirects every request to the
  canonical host instead of serving.

## Develop

```sh
cd web
npm install
npm run dev      # hot-reloading dev server
npm run build    # type-check + build → ../docs (content-hashed assets)
```

## Static root files

`web/public/` is copied verbatim into `docs/` on every build:
`CNAME` (the Pages custom domain), `og.png` (1200×630 social card),
`robots.txt`, `sitemap.xml` (homepage only — 0055 extends it for `/docs/`),
`favicon.svg`, `apple-touch-icon.png`.

## Deploy

The primary deploy is just committing the rebuilt `docs/` to `main` — GitHub
Pages redeploys automatically.

The Dibbla alias (legacy `https://cc-screen-<id>.dibbla.app` links):

```sh
./deploy.sh              # builds, then deploys the alias as a 301 → ccscreen.dev
./deploy.sh "msg"        # with a custom commit message
SITE_CANONICAL="" ./deploy.sh   # make the alias serve the site itself again
```

## CI

`.github/workflows/site-ci.yml` builds the site on PRs touching `site/**` or
`docs/**`, asserts the static root files land in `docs/`, checks the
`emptyOutDir` contract with a sentinel in `docs/docs/`, and link-checks the
built page (skipping `app.ccscreen.dev` until 0053's cutover is live).

## Local check

```sh
( cd web && npm install && npm run build )   # -> ../docs
cp -R ../docs/. public/
docker build -t cc-screen-site .
docker run --rm -p 8080:8080 cc-screen-site  # -> http://localhost:8080
```
