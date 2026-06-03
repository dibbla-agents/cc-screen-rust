---
name: release
description: How to ship a cc-screen-rust update — update the running server, deploy the docs site, and cut + host a new installable release (version bump → tag/CI build → host the curl|sh installer + binaries on the Dibbla site). Use for "ship it", "release", "publish a version", "update the installer", "deploy the docs", "bump the version".
when_to_use: The user wants to release/ship/publish a new version, update the install one-liner that `curl … | sh` serves, deploy the docs site, update the running cc-screen-rust service, bump the version, or otherwise get changes out to users or to their own box. Also when they ask "what do I do after an update?".
---

# Shipping a cc-screen-rust update

There are **three independent things** you can "update" here. Figure out which the
user means (ask if ambiguous) — they don't all need doing every time.

| You want to… | Do this | Touches |
|---|---|---|
| Run the new code on the box **you** attach to | `./install.sh` | the systemd `--user` service on port 8839 |
| Publish the docs site changes | `site/deploy.sh` | the Dibbla `cc-screen` app (the website) |
| Make `curl … \| sh` install the new version | bump → `./release.sh` → `site/release-host.sh` | the GitHub release **and** the Dibbla-hosted installer/binaries |

These are decoupled: shipping a release does **not** update your running server, and
updating your server does **not** publish anything. Most day-to-day "I changed
something" cases are just `./install.sh`.

All commands run from the repo root unless noted. `cargo` may not be on `PATH`;
the scripts `. "$HOME/.cargo/env"` themselves.

## A. Update the running server (the common case)

The service you attach to from your phone/TUI. After committing your changes:

```sh
./install.sh                 # rebuild frontend + binary, restart the service
# flags: -p PORT · --bind ADDR · --no-restore · --no-build · --no-service
```

It rebuilds the embedded frontend and the release binary, then restarts the
`cc-screen-rust` systemd `--user` service (auto-resumes sessions). **This is the
whole thing for a self-only update — no release needed.** Bouncing the service
ends the agents momentarily; auto-restore relaunches each with its resume flag.

## B. Deploy the docs site

The landing/getting-started site (`site/`, a Vite+React app served by a tiny
axum static server on Dibbla as the `cc-screen` app).

```sh
cd site && ./deploy.sh "optional commit message"
```

It builds `web/` → `../docs` → `public/` and `dibbla deploy`s (zero-downtime
update). Asset cache-busting is automatic (content-hashed filenames). Note: the
hosted installer/binaries under `/dl` are layered in by the Dockerfile from
`site/dl/`, so a docs-only deploy keeps them **as long as `site/dl/` is still
populated** from the last `release-host.sh` run (don't deploy the site from a
fresh checkout where `site/dl/` is empty — it would drop the installers).

## C. Cut + host a new installable release

This is what makes the README's `curl … | sh` one-liners serve a new version.
The install is hosted on **our own domain** (`https://cc-screen-b4687da9.dibbla.app/dl`),
not GitHub — the binaries are the CI-built artifacts, re-hosted on the Dibbla site.

Confirm with the user before tagging/publishing — these steps push a tag, publish
a GitHub release, and deploy the site (all outward/visible).

1. **Bump the version** (lockstep across the 3 crates) and commit:
   ```sh
   ./bump.sh 0.2.3              # edits the 3 Cargo.toml + refreshes Cargo.lock, stages them
   git commit -m "Release 0.2.3" && git push
   ```
   (Manual equivalent: edit `version` in `Cargo.toml`, `crates/tui/Cargo.toml`,
   `crates/protocol/Cargo.toml`, then `cargo check --workspace` to refresh the lock.)

2. **Tag + build + GitHub mirror:**
   ```sh
   ./release.sh                 # reads the version from Cargo.toml, tags vX.Y.Z, pushes,
                                # waits for the CI cross-build, publishes the GitHub release
   ```
   `release.sh` handles the org's read-only-token quirk (CI builds the artifacts
   but its own "Create GitHub Release" step 403s, so the script publishes from the
   built artifacts with the user's `gh` creds; no-ops if CI already published).
   The "host" CI job failing is expected — the build artifacts upload before it.

3. **Host the installer + binaries on Dibbla:**
   ```sh
   site/release-host.sh         # no arg = latest tag; or: site/release-host.sh v0.2.3
   ```
   Pulls the CI artifacts (`gh run download`), rewrites the cargo-dist installers
   to fetch from the Dibbla origin (embedded checksums + OS/arch detection are
   left intact), stages them under `site/dl/<version>/`, and deploys the site.

4. **(Optional) update your own running server** to the new code: `./install.sh`.

### Verify after C

```sh
B=https://cc-screen-b4687da9.dibbla.app
curl -s $B/dl/version                                   # → vX.Y.Z
curl -s $B/dl/install-ccs.sh | grep APP_VERSION         # → APP_VERSION="X.Y.Z"
curl -sI $B/dl/v<VERSION>/cc-screen-tui-x86_64-unknown-linux-musl.tar.xz | head -1   # → HTTP 200
# definitive end-to-end (installs/updates ccs locally):
curl --proto '=https' --tlsv1.2 -LsSf $B/dl/install-ccs.sh | sh && ccs --version
```

## Install one-liners (what users run)

```sh
# ccs client
curl --proto '=https' --tlsv1.2 -LsSf https://cc-screen-b4687da9.dibbla.app/dl/install-ccs.sh | sh
# server (then `cc-screen-rust install` to wire the service)
curl --proto '=https' --tlsv1.2 -LsSf https://cc-screen-b4687da9.dibbla.app/dl/install-cc-screen.sh | sh
```

## Gotchas

- **CDN cache (~4h):** versioned tarball URLs (`/dl/<version>/…`) are instant, but
  the stable `/dl/install-*.sh` script can serve the *previous* version for up to
  ~4h after `release-host.sh` until the cache TTL rolls. Fine for a personal tool;
  don't expect a brand-new release to be installable the same second you host it.
- **The version bump is the one manual decision.** `release.sh` reads the version
  from `Cargo.toml`; it does not bump it. Use `./bump.sh X.Y.Z` first.
- **No custom domain** — installs run off the `cc-screen-b4687da9.dibbla.app`
  hostname. That's expected; nothing to do.
- **Don't `site/deploy.sh` from a fresh checkout** (empty `site/dl/`) — it would
  ship a site without the installers. Run `site/release-host.sh` (which repopulates
  `site/dl/`) when in doubt.
- `site/dl/` binaries are gitignored (regenerated per release); only the latest
  version is kept to bound the site image size.

## Key files

- `install.sh` — build + (re)install the running server's service.
- `bump.sh` — lockstep version bump (3 crates + Cargo.lock).
- `release.sh` — tag + wait for CI + publish the GitHub release.
- `site/release-host.sh` — re-host the CI artifacts on the Dibbla site under `/dl`.
- `site/deploy.sh` — deploy the docs site.
- `dist-workspace.toml` — cargo-dist config (targets, installers, lockstep versions).
