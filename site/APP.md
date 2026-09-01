---
subtitle: The cc-screen landing site — the front door for the hosted SaaS and the self-host track.
---

# cc-screen — landing site

This app serves the **cc-screen** landing page: the marketing/acquisition site
for the hosted service at [app.ccscreen.dev](https://app.ccscreen.dev), with a
self-host guide for people who run their own hub. The canonical home is
**https://ccscreen.dev** (GitHub Pages); this Dibbla deployment survives as a
redirect for legacy links.

## What you'll find here

- **What cc-screen is** — keep a team of AI coding agents (Claude, Codex,
  Gemini, Kimi, OpenCode, Grok) running around the clock on your own machines, and check in on
  any of them from your phone or laptop, with real file cowork.
- **How it works in 3 steps** — sign up at app.ccscreen.dev, paste one install
  one-liner on your dev box, type the short code it prints. Free during the
  beta, no card.
- **Features & apps** — the phone PWA, the browser app, and the native `ccs`
  terminal client.
- **Demo** — a short terminal recording of a machine coming online.
- **Pricing** — the free/pro/unlimited plans; everything is free during the
  beta.
- **Self-host** — copy-paste install commands for running the hub and machines
  yourself: same product, your box, your network.
- **Docs** — the end-user documentation lives under `/docs/`.

## How to use it

It's a single page: read the intro, follow **Sign up** to create an account on
the hosted hub, or jump to **Self-host** for the run-it-yourself commands (each
has a copy button). **GitHub** has the source and releases.

## Notes

- The page itself is static and public. Accounts live on the hosted hub at
  app.ccscreen.dev (there's a free plan, no card required); self-hosting
  needs no account at all.
- The canonical source for this content lives in `site/web/` of the
  [cc-screen-rust repository](https://github.com/dibbla-agents/cc-screen-rust);
  the built page is committed under `docs/` and served by GitHub Pages at
  https://ccscreen.dev — this app serves (or redirects to) a copy of it.
