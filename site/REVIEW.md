---
Review-status: Warnings
One-Sentence-Summary: "Static, read-only public docs server (Rust/axum) — no secrets, DB, user input or outbound calls; only finding is no security headers (CSP), optional for a static marketing page."
---

## Pre-deploy guardrails report

- [x] **Security (OWASP Top 10):** OK — 1 minor warning
  - No hardcoded secrets; no `.env` in the deploy dir (git/dockerignored).
  - No SQL / command injection or XSS surface — the server only serves static,
    author-authored files; tower-http `ServeDir` blocks path-traversal escapes.
  - WARNING: no security headers (e.g. `Content-Security-Policy`). Low risk for a
    static public docs page with no auth, cookies, or user data; optional to add.
- [x] **Database usage:** N/A — no database.
- [x] **REST / API calls:** N/A — the server makes no outbound calls.
- [x] **External writes:** N/A — read-only static file server.
- Check 5 (URL task files): N/A.
- Check 6 (multi-service manifest): N/A — single Dockerfile, no `dibbla.yaml`.

**This change:** rebuilt the hand-written page as a **Vite + React + TypeScript +
Tailwind** app (source in `site/web/`, build output committed to `docs/`), and
tightened the responsive layout / image alignment. Still purely static content —
the Rust `ServeDir` server is unchanged; the build is a local dev-time step (Node
is never in the deploy image or the container). Output is now content-hashed
assets under `docs/assets/` (JS, CSS, the 8 screenshots) instead of plain
`styles.css`/`app.js`; `web/` and `tmp-images/` are excluded from the deploy
context via `.dockerignore`/`.dibblaignore`. No new runtime code paths, network or
DB surface.

**Compatibility:** Dockerfile at deploy root ✓ · binds `0.0.0.0:$PORT` (8080),
`EXPOSE 8080`, deploy `--port 8080` ✓ · non-root `USER app` ✓ · no secrets baked
in ✓ · ephemeral-fs safe (static files live in the image) ✓. Smoke-tested — `/`,
`index.html`, the hashed JS/CSS bundles and all 8 `assets/*.png` return 200 (local
and on the live Dibbla URL); layout verified in headless Chromium at mobile
(390px) and desktop (1280px) widths.

**Result: OK to deploy** — 1 optional warning (security headers).
