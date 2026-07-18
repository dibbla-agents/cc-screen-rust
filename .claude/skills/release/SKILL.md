---
name: release
description: How to ship a cc-screen-rust update — update the running server, deploy the docs site, and cut a new installable release (version bump → tag/CI build → the curl|sh installers serve straight from the GitHub Release). Use for "ship it", "release", "publish a version", "update the installer", "deploy the docs", "bump the version".
when_to_use: The user wants to release/ship/publish a new version, update the install one-liner that `curl … | sh` serves, deploy the docs site, update the running cc-screen-rust service, bump the version, or otherwise get changes out to users or to their own box. Also when they ask "what do I do after an update?".
---

# Shipping a cc-screen-rust update

There are **three independent things** you can "update" here. Figure out which the
user means (ask if ambiguous) — they don't all need doing every time.

| You want to… | Do this | Touches |
|---|---|---|
| Run the new code on the box **you** attach to | `./install.sh` | the systemd `--user` service on port 8839 |
| Publish the docs site changes | `site/deploy.sh` | the Dibbla `cc-screen` app (the website) |
| Make `curl … \| sh` install the new version | bump → `./release.sh` | the GitHub Release (the installers serve from `releases/latest`) |

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
axum static server on Dibbla as the `cc-screen` app). **Docs only** — it no longer
hosts any binaries (those come straight from the GitHub Release), so it's safe to
deploy from any checkout.

```sh
cd site && ./deploy.sh "optional commit message"
```

It builds `web/` → `../docs` → `public/` and `dibbla deploy`s (zero-downtime
update). Asset cache-busting is automatic (content-hashed filenames).

## C. Cut a new installable release

This is what makes the README's `curl … | sh` one-liners serve a new version. The
installers are the cargo-dist scripts **served straight from the GitHub Release**
(`releases/latest/download/<pkg>-installer.sh`). The repo is public, so they
download anonymously — there is **no Dibbla `/dl` mirror** to refresh. Publishing
the GitHub Release *is* the whole thing.

Confirm with the user before tagging/publishing — these steps push a tag and
publish a GitHub release (outward/visible).

1. **Bump the version** (lockstep across all crates) and commit:
   ```sh
   ./bump.sh 0.2.3              # edits every workspace Cargo.toml + refreshes Cargo.lock, stages them
   git commit -m "Release 0.2.3" && git push
   ```
   (Manual equivalent: edit `version` in each workspace `Cargo.toml`, then
   `cargo check --workspace` to refresh the lock.)

2. **Tag + build + publish:**
   ```sh
   ./release.sh                 # reads the version from Cargo.toml, tags vX.Y.Z, pushes,
                                # waits for the CI cross-build, publishes the GitHub release
   ```
   CI cross-builds all targets (macOS arm64/x64, Linux arm64/x64 musl, Windows
   msvc) and, as of the org-token fix, publishes the GitHub Release itself.
   `release.sh` detects that and no-ops; if CI's publish ever regresses it
   publishes from the built artifacts with the user's `gh` creds instead.
   `releases/latest` repoints on publish, so the one-liners serve the new version
   immediately (no CDN mirror in the path).

3. **(Optional) update your own running server** to the new code: `./install.sh`.

### Verify after C

```sh
B=https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download
curl -fsSL -o /dev/null -w '%{http_code}\n' -L $B/cc-screen-rust-installer.sh   # → 200
curl -fsSL $B/cc-screen-tui-installer.sh | grep app_version                      # → app_version = 'X.Y.Z'
# definitive end-to-end (installs/updates ccs locally):
curl --proto '=https' --tlsv1.2 -LsSf $B/cc-screen-tui-installer.sh | sh && ccs --version
```

## Install one-liners (what users run)

Served from `releases/latest`, so they always fetch the newest tag. The asset
name is the cargo-dist `<pkg>-installer.sh` (note `ccs` ships from the
`cc-screen-tui` package):

```sh
# ccs client
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-tui-installer.sh | sh
# server (then `cc-screen-rust install` to wire the service)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.sh | sh
# hub
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-hub-installer.sh | sh
# Windows agent (PowerShell)
irm https://github.com/dibbla-agents/cc-screen-rust/releases/latest/download/cc-screen-rust-installer.ps1 | iex
```

The `update` subcommands (`cc-screen-rust update`, `ccs update`, `cc-screen-hub
update`) re-run these same installers; the base URL is the single-source-of-truth
`RELEASE_BASE_URL` in `crates/protocol/src/lib.rs`.

The Windows machine install through the hub (`irm <hub>/install.ps1 | iex`) also
resolves to `releases/latest` — the hub bakes `DEFAULT_INSTALLER_PS1_URL`
(`crates/hub/src/install.rs`) into the served `install-machine.ps1`, overridable
per-hub with `CCHUB_INSTALLER_PS1_URL` / `CCHUB_INSTALLER_URL`.

## Gotchas

- **The version bump is the one manual decision.** `release.sh` reads the version
  from `Cargo.toml`; it does not bump it. Use `./bump.sh X.Y.Z` first.
- **Public repo is load-bearing.** Anonymous `curl | sh` from a GitHub Release only
  works because `dibbla-agents/cc-screen-rust` is public; a private repo 404s
  anonymous asset downloads. If it's ever made private, the one-liners break (that
  was the original reason for the old `/dl` mirror, since removed).
- **`releases/latest` is instant** — no CDN mirror to wait on. A published release
  is installable immediately.

## Key files

- `install.sh` — build + (re)install the running server's service.
- `bump.sh` — lockstep version bump (all crates + Cargo.lock).
- `release.sh` — tag + wait for CI + publish the GitHub release.
- `site/deploy.sh` — deploy the docs site (docs only; no binaries).
- `crates/protocol/src/lib.rs` — `RELEASE_BASE_URL` (the installer origin the
  `update` subcommands use).
- `dist-workspace.toml` — cargo-dist config (targets, installers, lockstep versions).
