# cc-screen-site

The cc-screen getting-started / landing page.

- **`web/`** — the source: a **Vite + React + TypeScript + Tailwind** app.
- **`../docs`** — the build output, committed so **GitHub Pages** can serve it
  (from `/docs` on `main`).
- **this crate** — a tiny Rust (axum + tower-http) static-file server that serves
  a copy of the build on **Dibbla** as the `cc-screen` app, from a small, non-root
  Alpine container.

## Develop

```sh
cd web
npm install
npm run dev      # hot-reloading dev server
npm run build    # type-check + build → ../docs (content-hashed assets)
```

## Deploy

```sh
./deploy.sh              # builds web/ → ../docs, syncs to ./public, `dibbla deploy`
./deploy.sh "msg"        # with a custom commit message
```

`deploy.sh` builds the site, then creates the app on first run and does a
zero-downtime `--update` after that. The site is live at
`https://cc-screen-<id>.dibbla.app`. Cache-busting is automatic — Vite
content-hashes every asset, so changed files ship immediately while unchanged
ones keep their long CDN cache.

## Local check

```sh
( cd web && npm install && npm run build )   # -> ../docs
cp -R ../docs/. public/
docker build -t cc-screen-site .
docker run --rm -p 8080:8080 cc-screen-site  # -> http://localhost:8080
```
