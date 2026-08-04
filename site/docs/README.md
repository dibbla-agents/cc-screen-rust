# site/docs — the end-user documentation sources (proposal 0055)

These markdown files render to `https://ccscreen.dev/docs/…` via
`site/web/scripts/build-docs.mjs`, which emits `docs/docs/<slug>/index.html`.

**The build-order invariant:** `vite build` wipes `docs/`, so the docs
generator must run *after* it — `npm run build` (in `site/web`) does the whole
thing in order. If you ran a bare `vite build` and `/docs` vanished,
`npm run docs` restores it in seconds. `npm run check-links` verifies there
are no dead internal links in the emitted tree.

Rules of the road (Part C of the proposal):

- every command must be copy-paste-real (`app.ccscreen.dev` literally, never
  a `<hub>` placeholder on SaaS pages; `<angle-brackets>` only for
  user-chosen values, always with an inline example);
- docs link each other **relatively** (`../security/`), the product
  **absolutely** (`https://app.ccscreen.dev/...`);
- pages listed in `nav.json` and `.md` files here must match 1:1 — the build
  fails otherwise (this `README.md` is the one exemption);
- screenshots live in `img/` and are copied verbatim.
