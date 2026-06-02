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

**Compatibility:** Dockerfile at deploy root ✓ · binds `0.0.0.0:$PORT` (8080),
`EXPOSE 8080`, deploy `--port 8080` ✓ · non-root `USER app` ✓ · no secrets baked
in ✓ · ephemeral-fs safe (static files live in the image) ✓. Built & smoke-tested
locally — 13.9 MB image; `/`, `styles.css`, `app.js` all 200.

**Result: OK to deploy** — 1 optional warning (security headers).
