// The capture manifest — the single source of truth for every screenshot,
// clip, and cast the docs and the landing page embed (proposal 0061 A1).
//
// One entry per asset. The runner (capture.mjs) reads this; the docs asset
// check (site/web/scripts/check-links.mjs) cross-references it; humans grep
// `usedBy` to answer "what do I re-shoot after this UI change?".
//
// Entry shape:
//   id        unique kebab-case name, house style `<surface>-<subject>`
//             (surfaces: web- / mobile- / tui- / docs-)
//   kind      'still'  — Playwright screenshot, engine picked by env (A2)
//             'clip'   — Playwright recordVideo → WebM + MP4 + poster PNG
//             'cast'   — asciinema v2 recording, made by hand against the
//                        real installer; the runner only verifies + budgets
//             'manual' — a shot the pipeline can't drive (real TTY / real
//                        terminal emulator); verified + budgeted only
//             'static' — hand-authored asset (SVG); verified only
//   url       page to open (stills/clips)
//   viewport  CSS-pixel viewport; `scale` = deviceScaleFactor (default 2)
//   auth      'demo' → use the staged demo session (.auth/state.json, A4)
//             'none' → fresh context, logged out
//   steps     scripted interactions before capture; each step is one of
//             { fill: [sel, value] }  { click: sel }  { waitFor: sel }
//             { wait: ms }  { press: key }  { hover: sel }  { eval: js }
//   capture   optional CSS selector — screenshot that element, not the page
//   out       repo-relative output path(s). Listing both the docs and the
//             marketing path means ONE capture, TWO writes — the drift
//             channel 0055 opened is closed here, not by convention.
//   poster    (clips) PNG written from the clip's settled opening frame
//             (~1.2 s in — frame 0 predates the first paint)
//   usedBy    docs pages / components that embed the asset (greppable, and
//             enforced by check-links.mjs: no orphans in either direction)
//   blockedOn skip this entry, report why (kept as recorded intent)
//   note      free-text context for the next person
//
// Budgets (A5, enforced by the runner): still ≤ 350 KB · clip ≤ 15 s and
// ≤ 2.5 MB per file · cast ≤ 20 s and ≤ 5 KB · svg ≤ 100 KB.

const APP = "https://app.ccscreen.dev";

export default [
  // ── Part B — docs stills ─────────────────────────────────────────────

  {
    id: "web-signup",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 880, height: 700 },
    scale: 2,
    auth: "none",
    steps: [{ waitFor: "text=Continue with Google" }],
    out: ["site/docs/img/web-signup.png"],
    usedBy: ["quickstart.md"],
    note: "Sign-in / create-account screen with the Google button (B7).",
  },
  {
    id: "web-activate",
    kind: "still",
    url: `${APP}/activate`,
    viewport: { width: 880, height: 700 },
    scale: 2,
    auth: "demo",
    steps: [{ fill: ["input", "WDJB-MJHT"] }],
    out: ["site/docs/img/web-activate.png"],
    usedBy: ["quickstart.md", "install-macos-linux.md", "install-windows.md"],
    note:
      "The /activate approval screen with a code typed in (B1). Full-viewport " +
      "shot (the page has no <main>; the Backdrop atmosphere is the frame).",
  },
  {
    id: "web-add-machine",
    kind: "still",
    // /billing/cancel is the one deep link that lands on the Dashboard
    // (App.tsx routes it there with no billing notice) — `/` opens the
    // terminal, not the dashboard.
    url: `${APP}/billing/cancel`,
    viewport: { width: 1440, height: 900 },
    scale: 2,
    auth: "demo",
    steps: [
      { waitFor: "text=Add a machine" },
      // Name the box like the staged Windows machine so the generated
      // command reads real: irm https://app.ccscreen.dev/install.ps1?name=harebell | iex
      { fill: ["input[placeholder=my-laptop]", "harebell"] },
      { click: "text=Windows (PowerShell)" },
    ],
    // The add-machine Window card (its root is the only .rounded-2xl
    // containing this heading).
    capture: ".rounded-2xl:has-text('Add a machine')",
    out: ["site/docs/img/web-add-machine.png"],
    usedBy: [
      "quickstart.md",
      "install-macos-linux.md",
      "install-windows.md",
      "web-app.md",
    ],
    note: "The Add-a-machine card, Windows tab selected (B2).",
  },
  {
    id: "web-machines",
    kind: "still",
    url: `${APP}/billing/cancel`, // Dashboard deep link — see web-add-machine
    viewport: { width: 1440, height: 900 },
    scale: 2,
    auth: "demo",
    steps: [{ waitFor: "text=harebell" }, { wait: 800 }],
    out: ["site/docs/img/web-machines.png"],
    usedBy: ["web-app.md", "troubleshooting.md"],
    note:
      "Machines dashboard: pine online (pulsing amber dot) with the " +
      "'3 missing · Install' affordance, harebell offline, plus the plan card " +
      "(B3). Known wart: pine's six action buttons overflow the max-w-lg card " +
      "and crush the machine name (MachineRow has no flex-wrap) — reshoot " +
      "after that's fixed. Also reshoot after 0058's plan reprice one-shot " +
      "runs in prod: the plan card still shows the pre-reprice Free caps " +
      "(10 machines / 50 sessions) instead of the approved 2 / 5.",
  },
  {
    id: "web-grid",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 1440, height: 900 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/web-grid.mjs — Ctrl+B chords: layout 3, mount 3 panes, palette open
    quantize: true, // 3 dense terminal panes ≈ 690 KB as RGB; 8-bit fits budget
    out: ["site/docs/img/web-grid.png"],
    usedBy: ["web-app.md"],
    note:
      "Desktop grid, Left-tall-L layout: Web app + Docs site + deploy panes " +
      "all live, with the layout palette open under the header (B4). All " +
      "staged sessions live on pine — harebell is offline by design, so a " +
      "cross-machine grid isn't honestly stageable on the Free scene.",
  },
  {
    id: "web-sharing",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 1440, height: 900 },
    scale: 2,
    auth: "demo",
    steps: [],
    out: ["site/docs/img/web-sharing.png"],
    usedBy: ["web-app.md"],
    blockedOn: "plan",
    note:
      "Invite → bell inbox → Sharing card (B6). Free tier has " +
      "can_create_shares=false; needs the demo account on a share-capable " +
      "plan before this can frame a real invite.",
  },
  {
    id: "mobile-notifications",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 460, height: 400 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/mobile-notifications.mjs
    out: ["site/docs/img/mobile-notifications.png"],
    usedBy: ["web-app.md", "troubleshooting.md", "faq.md"],
    note:
      "The Web Push bell in the switcher header, hovered, on a phone-width " +
      "crop (B8). The enabled/filled state is unreachable from automation " +
      "(Chromium's push provider ignores CDP permission overrides — " +
      "subscribe() aborts), so the resting bell + caption carries it.",
  },
  {
    id: "tui-switcher",
    kind: "manual",
    out: ["site/docs/img/tui-switcher.png"],
    usedBy: ["tui.md"],
    note:
      "The ccs full-screen switcher over the staged demo scene (B5). ccs " +
      "needs a real TTY, so the recipe is tui-shot.sh: ccs in a 132x10 tmux " +
      "pane (isolated HOME, signed in to the demo account via `ccs activate` " +
      "+ POST /api/device/approve), clear the boot box to land in the " +
      "switcher, then `tui-shot.sh snap … --cols 132 --font-size 10.7 " +
      "--line-height 16` (tmux capture-pane -e → ansi2png.mjs → 2x PNG, " +
      "pngquant).",
  },
  {
    id: "tui-layouts",
    kind: "manual",
    out: ["site/web/src/assets/img/tui-layouts.png", "site/docs/img/tui-layouts.png"],
    usedBy: ["tui.md"],
    note:
      "The layout palette (Ctrl-A l) over the demo grid. Re-shot fresh " +
      "(0061; the old marketing orphan pre-dated 0060) and adopted by " +
      "tui.md's layouts section. Recipe: same 212x53 tmux scene as tui-grid " +
      "with the palette open, then `tui-shot.sh snap … --cols 212`.",
  },
  {
    id: "ccs-activate",
    kind: "manual",
    out: ["site/docs/img/ccs-activate.png"],
    usedBy: ["tui.md"],
    note:
      "ccs activate: the one-time code, the URL, the signed-in confirmation " +
      "(0060). Pre-dates this manifest; adopted per the no-orphans rule — " +
      "real TTY, captured from a real terminal.",
  },
  {
    id: "web-approve-terminal",
    kind: "manual",
    out: ["site/docs/img/web-approve-terminal.png"],
    usedBy: ["tui.md"],
    note:
      "Approving the terminal's code on /activate (0060). Pre-dates this " +
      "manifest; adopted per the no-orphans rule. Could graduate to a " +
      "scripted still once staging can mint a pending device code.",
  },
  {
    id: "docs-topology",
    kind: "static",
    out: ["site/docs/img/docs-topology.svg"],
    usedBy: ["index.md", "self-hosting.md", "security.md"],
    note:
      "Hand-authored rendered version of the index.md ASCII architecture " +
      "diagram — hub / machines / clients, dial-out arrows, house colors (B9).",
  },
  {
    id: "web-upgrade",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 880, height: 700 },
    scale: 2,
    auth: "demo",
    steps: [],
    out: ["site/docs/img/web-upgrade.png"],
    usedBy: [],
    blockedOn: "0058",
    note:
      "The plan-limit / upgrade dialog. Referenced three times in docs prose " +
      "but its UI is being reworked by 0058 — capturing now guarantees rot.",
  },

  // ── Part C — motion ──────────────────────────────────────────────────

  {
    id: "install",
    kind: "cast",
    out: [
      "site/web/src/assets/demo/install.cast",
      "site/docs/media/install.cast",
    ],
    usedBy: ["quickstart.md", "install-macos-linux.md", "Demo.tsx"],
    note:
      "The macOS/Linux install one-liner, recorded with asciinema against " +
      "the real installer (C1; pre-existing landing asset, now also in docs).",
  },
  {
    id: "install-windows",
    kind: "cast",
    out: [
      "site/web/src/assets/demo/install-windows.cast",
      "site/docs/media/install-windows.cast",
    ],
    usedBy: ["install-windows.md", "Demo.tsx"],
    note:
      "The PowerShell one-liner on the real Windows bench (harebell), " +
      "recorded to an asciinema v2 cast (C2).",
  },
  {
    id: "web-activate-clip",
    kind: "clip",
    url: `${APP}/activate`,
    viewport: { width: 1280, height: 720 },
    auth: "demo",
    steps: [],
    out: [
      "site/docs/media/web-activate.webm",
      "site/docs/media/web-activate.mp4",
    ],
    poster: "site/docs/media/web-activate-poster.png",
    usedBy: ["quickstart.md"],
    note:
      "Type code → Approve → harebell's dashboard dot flips online, ~11 s, " +
      "all real (C3). drivers/web-activate-clip.mjs mints a pending device " +
      "code over HTTP, approves it on camera, then starts a real short-lived " +
      "agent so the flip is genuine; it rotates .auth/uplink-harebell.json " +
      "and leaves harebell offline again (verify with stage.mjs --check).",
  },
  {
    id: "web-create-session",
    kind: "clip",
    url: `${APP}/`,
    viewport: { width: 1280, height: 720 },
    auth: "demo",
    steps: [],
    out: [
      "site/docs/media/web-create-session.webm",
      "site/docs/media/web-create-session.mp4",
      "site/web/src/assets/demo/web-create-session.webm",
      "site/web/src/assets/demo/web-create-session.mp4",
      // The Tour's poster copy — deliver() routes any out path containing
      // "poster" from the poster frame, so one capture writes both copies.
      "site/web/src/assets/demo/web-create-session-poster.png",
    ],
    poster: "site/docs/media/web-create-session-poster.png",
    usedBy: ["web-app.md", "Tour.tsx"],
    trim: 7.9,
    freeze: 1.5,
    note:
      "New session → directory search ('web' → ~/web-app) → named 'api' → " +
      "Create → the pane opens and Claude boots to its (demo-branded) " +
      "banner (C4). drivers/web-create-session.mjs; it leaves a real " +
      "claude-api session on pine — run stage.mjs afterwards to restore the " +
      "scene. One capture, two consumers: docs and the landing Tour. Why " +
      "`trim`: the demo HOME symlinks erik's real ~/.claude/.credentials" +
      ".json, and Claude's startup account-sync overwrites the spoofed " +
      "oauthAccount (email + orgName) ~3 s after boot; the first post-sync " +
      "redraw (the session footer bar appearing shrinks the PTY a row → " +
      "full repaint, ~8.1 s into the recording) would put the personal " +
      "identity back in the boot banner on camera. The delivery is cut at " +
      "7.9 s (+1.5 s freeze on the last clean frame) — Claude booted, " +
      "banner demo-branded, repaint never shown. Remove trim/freeze only " +
      "after enrolling real demo-account Anthropic credentials in the " +
      "demo HOME.",
  },

  // ── Part E1 — marketing re-captures (same names, same w/h contract) ──

  {
    id: "mobile-sessions",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 460, height: 783 },
    scale: 2,
    auth: "demo",
    steps: [{ waitFor: "text=Docs site" }, { wait: 1200 }],
    out: [
      "site/web/src/assets/img/mobile-sessions.png",
      "site/docs/img/mobile-sessions.png",
    ],
    usedBy: ["index.md", "Tour.tsx"],
    note:
      "Session switcher on a phone (auto-opens when no session is attached). " +
      "All sessions live on pine — offline harebell has none by design, so " +
      "'both machines' isn't honestly stageable on the Free scene.",
  },
  {
    id: "mobile-agent",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 460, height: 783 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/mobile-agent.mjs
    out: [
      "site/web/src/assets/img/mobile-agent.png",
      "site/docs/img/mobile-agent.png",
    ],
    usedBy: ["quickstart.md", "Tour.tsx"],
    note: "The Web app claude session live on a phone, key bar in frame.",
  },
  {
    id: "mobile-files",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 460, height: 783 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/mobile-files.mjs
    out: ["site/web/src/assets/img/mobile-files.png"],
    usedBy: ["Tour.tsx"],
    note: "The file browser on a phone, in the web-app dir (marketing only).",
  },
  {
    id: "mobile-editor",
    kind: "still",
    url: `${APP}/`,
    viewport: { width: 460, height: 783 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/mobile-editor.mjs
    out: [
      "site/docs/img/mobile-editor.png",
      "site/web/src/assets/img/mobile-editor.png",
    ],
    usedBy: ["web-app.md"],
    note: "src/App.tsx open in the editor on a phone.",
  },
  {
    id: "web-new-session",
    kind: "still",
    url: `${APP}/`,
    // 640 tall: the sidebar spans the full viewport height, and at 900 the
    // create panel is mostly empty space between the folder list and the
    // pinned tool-pill footer.
    viewport: { width: 1440, height: 640 },
    scale: 2,
    auth: "demo",
    steps: [], // drivers/web-new-session.mjs — ☰ → sidebar New session → search "web"
    capture: ".inset-y-0.left-0", // the sidebar switcher panel (create view)
    out: [
      "site/docs/img/web-new-session.png",
      "site/web/src/assets/img/web-new-session.png",
    ],
    usedBy: ["web-app.md"],
    note:
      "The create-session flow in the sidebar — machine picker, directory " +
      "search, tool pills, name, Create (it's an in-drawer view, not a " +
      "[role=dialog] modal).",
  },
  {
    id: "web-cowork",
    kind: "still",
    url: `${APP}/`,
    // ≥900 CSS px or the app renders its phone UI (useIsDesktop gates on
    // min-width: 900px) and the agent-mirror column doesn't exist; 1000 also
    // gives the three columns (tree / file / agent) honest breathing room.
    viewport: { width: 1000, height: 560 },
    scale: 2,
    auth: "demo",
    quantize: true, // markdown + live terminal mirror — RGB rides the budget line
    steps: [], // drivers/web-cowork.mjs — Files overlay + README.md + agent mirror
    out: [
      "site/docs/img/web-cowork.png",
      "site/web/src/assets/img/web-cowork.png",
    ],
    usedBy: ["web-app.md", "Tour.tsx"],
    note:
      "Coworking: the editor overlay (tree + README.md) with the live Web " +
      "app claude session in the right-hand agent-mirror column.",
  },
  {
    id: "tui-grid",
    kind: "manual",
    out: [
      "site/docs/img/tui-grid.png",
      "site/web/src/assets/img/tui-grid.png",
    ],
    usedBy: ["tui.md", "Tour.tsx"],
    note:
      "The three demo sessions in the ccs left-L grid (re-shot for 0061 — " +
      "the old frame showed a pre-0015 session). Recipe: ccs in a 212x53 " +
      "tmux pane on the demo account, attach all three (Ctrl-A 4 + Ctrl-A d " +
      "fills), then `tui-shot.sh snap … --cols 212`.",
  },
];
