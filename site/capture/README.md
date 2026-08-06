# site/capture — the reproducible screenshot & clip pipeline

Every screenshot, clip, and cast the docs (`site/docs/`) and the landing page
(`site/web/`) embed is described by one entry in **`manifest.mjs`** and
regenerable with one command (proposal 0061). Shots stop rotting silently:
after a UI change, `usedBy` answers "what do I re-shoot?", and

```sh
node capture.mjs web-activate        # one asset
node capture.mjs --all               # everything not blocked
node capture.mjs --list              # manifest + on-disk state
```

rewrites the asset(s) with the same framing — same viewport, scale, theme,
and staged scene. Entries whose `out` lists both the docs and the marketing
path are written twice from one capture, so the two copies can't drift.

## One-time prerequisites

- `npm install` in this directory (pins `playwright` — the pin **must match
  the remote browser server's minor version**, a connect-time hard error
  otherwise).
- **ffmpeg** on the capture machine (`apt install ffmpeg`, or set `FFMPEG` /
  `FFPROBE`). Needed for clips only — Playwright's bundled binary can't
  filter or transcode.
- If recording clips through a remote browser server: `npx playwright
  install ffmpeg` once on the server host.

## Engines

- **Stills** connect to a real-Chrome Playwright browser server when
  `CAPTURE_WS_ENDPOINT` is set, else fall back to local headless Chromium
  (near-identical rendering; font fallback only). The endpoint URL contains
  its access secret — **env only, never committed**. Run such a server with:

  ```js
  import { chromium } from 'playwright';
  const srv = await chromium.launchServer({ channel: 'chrome', headless: false });
  console.log(srv.wsEndpoint());
  ```

- **Clips** record locally (`recordVideo`, 1280×720) and post-process with
  system ffmpeg into WebM + MP4 siblings plus a first-frame poster PNG.
- **Casts** are recorded with `asciinema rec` against the real installer and
  committed as v2 `.cast` files; **manual** entries (the `ccs` TUI needs a
  real TTY) are captured from a real terminal. For both, the runner verifies
  presence and budgets so they still fail loudly when missing or heavy.

Choreography beyond the manifest's step DSL lives in `drivers/<id>.mjs`
(`export default async (page, ctx) => {…}`).

## Budgets (binding — the runner fails the entry, never commits heavyweight)

| Kind | Limit |
|---|---|
| still / manual | PNG ≤ 350 KB |
| clip | ≤ 15 s, ≤ 2.5 MB per file (WebM and MP4 each) |
| cast | ≤ 20 s, ≤ 5 KB |
| static (SVG) | ≤ 100 KB |

## The demo account (the only identity ever in frame)

Captures run against **app.ccscreen.dev** as a dedicated demo account —
never a personal login, so no real session names, paths, or emails can leak
into a committed frame. `stage.mjs` asserts/repairs the scene (idempotent):

- machines `pine` (Linux, **online**) and `harebell` (Windows, **offline** —
  enrolled via the device flow but never connected, which is exactly what
  keeps its offline dot stable);
- three tidy sessions on pine (`web-app`, `docs-site`, `deploy`).

```sh
export CAPTURE_DEMO_EMAIL=… CAPTURE_DEMO_PASSWORD=…   # or .auth/credentials.json
node stage.mjs             # assert/repair + refresh .auth/state.json
node stage.mjs --check     # assert only
node stage.mjs --enroll M  # device-flow enrollment, prints the uplink token
```

`.auth/` (credentials, storageState, uplink tokens) is gitignored.

### Running the pine demo agent

The online machine is a real agent process with an **isolated `$HOME`** (so
the demo tenant never sees a personal filesystem):

```sh
DEMO_HOME=/home/erik/ccs-demo-home   # contains web-app/ and docs-site/ dirs
mkdir -p "$DEMO_HOME"/{web-app,docs-site,.local/bin}
HOME="$DEMO_HOME" CCWEB_HOME="$DEMO_HOME" \
  PATH=/usr/bin:/bin:"$DEMO_HOME"/.local/bin \
  CCWEB_HUB_URL=https://app.ccscreen.dev \
  CCWEB_HUB_TOKEN=$(jq -r .uplink_token .auth/uplink-pine.json) \
  cc-screen-rust --hub-only --machine-id pine
```

Both `HOME` *and* `CCWEB_HOME` must point at the demo home (the confine root
follows `CCWEB_HOME`), and session dirs are sent to the API as **absolute**
paths (`stage.mjs` reads `CAPTURE_DEMO_AGENT_HOME`, default the path above).
The restricted `PATH` (with only `claude` symlinked into
`$DEMO_HOME/.local/bin`) is deliberate: it makes the dashboard's
missing-assistants warning real for the `web-machines` shot.

Free-plan caps (2 machines / 5 sessions) fit the scene exactly — there is no
headroom, so don't enroll extras. The staged `harebell` sessions only exist
while it's online; the scene deliberately keeps all sessions on `pine`.

## Adding an asset

1. Add a manifest entry (`id`, `kind`, `url`, `viewport`, `steps`/driver,
   `out`, `usedBy`). House naming: `<surface>-<subject>` kebab-case,
   surfaces `web-` / `mobile-` / `tui-` / `docs-`.
2. `node capture.mjs <id>` until the frame is right.
3. Reference it from the page(s) named in `usedBy` — the docs asset check
   (`site/web/scripts/check-links.mjs`) fails CI on any drift between pages,
   disk, and manifest, in both directions.
4. Entries blocked on in-flight UI work carry `blockedOn: '00XX'` — kept as
   recorded intent, skipped by the runner, reported in the summary.
