// The capture runner (proposal 0061 A2/A5).
//
//   node capture.mjs <id…>          regenerate specific assets
//   node capture.mjs --all          regenerate everything not blocked
//   node capture.mjs --kind still   regenerate one kind
//   node capture.mjs --list         show the manifest and each asset's state
//
// Engines:
//   still — connects to a real-Chrome Playwright browser server when
//           CAPTURE_WS_ENDPOINT is set (the endpoint embeds its access
//           secret: env only, never committed), else falls back to local
//           headless Chromium. Dark theme, deviceScaleFactor from the
//           manifest, networkidle + settle delay.
//   clip  — local headless Chromium with recordVideo, then system ffmpeg
//           (FFMPEG env or PATH) into WebM + MP4 + first-frame poster.
//   cast / manual / static — not driven here (asciinema, a real TTY, or a
//           hand-authored SVG); the runner verifies existence and budgets
//           so a heavyweight or missing asset still fails loudly.
//
// Budgets are binding: an entry that exceeds its budget fails the run
// rather than committing a heavyweight asset silently.

import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  statSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import manifest from "./manifest.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", ".."); // repo root — manifest out paths are repo-relative
const AUTH_STATE = join(HERE, ".auth", "state.json");
const OUT_TMP = join(HERE, "out");

const BUDGET = {
  still: { bytes: 350 * 1024 },
  clip: { bytes: 2.5 * 1024 * 1024, seconds: 15 },
  cast: { bytes: 5 * 1024, seconds: 20 },
  manual: { bytes: 350 * 1024 },
  static: { bytes: 100 * 1024 },
};

// ── selection ────────────────────────────────────────────────────────────

const argv = process.argv.slice(2);
const flag = (f) => argv.includes(f);
const opt = (f) => {
  const i = argv.indexOf(f);
  return i >= 0 ? argv[i + 1] : undefined;
};
const ids = argv.filter((a) => !a.startsWith("--") && a !== opt("--kind"));

let selected;
if (flag("--list")) {
  for (const e of manifest) {
    const state = e.blockedOn
      ? `blocked on ${e.blockedOn}`
      : e.out.every((o) => existsSync(join(ROOT, o)))
        ? "present"
        : "MISSING";
    console.log(
      `${e.id.padEnd(24)} ${e.kind.padEnd(7)} ${state.padEnd(16)} → ${e.out.join(", ")}`,
    );
  }
  process.exit(0);
} else if (flag("--all")) {
  selected = manifest;
} else if (opt("--kind")) {
  selected = manifest.filter((e) => e.kind === opt("--kind"));
} else if (ids.length) {
  selected = ids.map((id) => {
    const e = manifest.find((m) => m.id === id);
    if (!e) {
      console.error(`no manifest entry: ${id}`);
      process.exit(2);
    }
    return e;
  });
} else {
  console.error("usage: node capture.mjs <id…> | --all | --kind <k> | --list");
  process.exit(2);
}

// ── engines ──────────────────────────────────────────────────────────────

let pw; // lazy playwright import — verify-only runs need no browser
async function browserFor(kind) {
  pw ??= await import("playwright");
  const ws = process.env.CAPTURE_WS_ENDPOINT;
  if (kind === "still" && ws) return pw.chromium.connect(ws);
  return pw.chromium.launch({ headless: true, args: ["--no-sandbox"] });
}

function contextOpts(entry) {
  const opts = {
    viewport: entry.viewport,
    deviceScaleFactor: entry.scale ?? 2,
    colorScheme: "dark",
  };
  if (entry.auth === "demo") {
    if (!existsSync(AUTH_STATE))
      throw new Error(
        `entry '${entry.id}' needs the demo login — run: node stage.mjs`,
      );
    opts.storageState = AUTH_STATE;
  }
  return opts;
}

async function runSteps(page, steps) {
  for (const s of steps ?? []) {
    if (s.fill) await page.locator(s.fill[0]).first().fill(s.fill[1]);
    else if (s.click) await page.locator(s.click).first().click();
    else if (s.waitFor) await page.locator(s.waitFor).first().waitFor();
    else if (s.wait) await page.waitForTimeout(s.wait);
    else if (s.press) await page.keyboard.press(s.press);
    else if (s.hover) await page.locator(s.hover).first().hover();
    else if (s.eval) await page.evaluate(s.eval);
    else throw new Error(`unknown step: ${JSON.stringify(s)}`);
  }
}

async function driverFor(entry) {
  // Choreography too rich for the step DSL lives in a sibling module:
  // drivers/<id>.mjs exporting `export default async (page, ctx) => {}`.
  const p = join(HERE, "drivers", `${entry.id}.mjs`);
  if (!existsSync(p)) return null;
  return (await import(p)).default;
}

async function captureStill(entry) {
  const browser = await browserFor("still");
  try {
    const ctx = await browser.newContext(contextOpts(entry));
    const page = await ctx.newPage();
    await page.goto(entry.url, { waitUntil: "networkidle" });
    const driver = await driverFor(entry);
    if (driver) await driver(page, ctx);
    else await runSteps(page, entry.steps);
    await page.waitForTimeout(entry.settle ?? 600);
    mkdirSync(OUT_TMP, { recursive: true });
    const tmp = join(OUT_TMP, `${entry.id}.png`);
    if (entry.capture)
      await page.locator(entry.capture).first().screenshot({ path: tmp });
    else await page.screenshot({ path: tmp, fullPage: !!entry.fullPage });
    await ctx.close();
    // Palette-quantize (visually lossless on flat UI/terminal frames — the
    // pre-0061 marketing shots are all 8-bit colormap for the same reason).
    // Opt-in per entry: dense multi-pane frames blow the 350 KB budget as
    // 24-bit RGB. pngquant must be on PATH (`apt install pngquant`).
    if (entry.quantize) {
      execFileSync("pngquant", ["--force", "--skip-if-larger", "--quality", "70-95", "--output", tmp, tmp], { stdio: "inherit" });
    }
    return tmp;
  } finally {
    await browser.close();
  }
}

function ffmpeg(args) {
  const bin = process.env.FFMPEG || "ffmpeg";
  execFileSync(bin, ["-y", "-hide_banner", ...args], {
    stdio: ["ignore", "ignore", "pipe"],
  });
}

function ffprobeSeconds(file) {
  const bin = process.env.FFPROBE || "ffprobe";
  const out = execFileSync(bin, [
    "-v", "error",
    "-show_entries", "format=duration",
    "-of", "csv=p=0",
    file,
  ]).toString().trim();
  return parseFloat(out);
}

async function captureClip(entry) {
  const browser = await browserFor("clip");
  const raw = join(OUT_TMP, `${entry.id}-raw`);
  mkdirSync(raw, { recursive: true });
  try {
    const ctx = await browser.newContext({
      ...contextOpts(entry),
      deviceScaleFactor: 1,
      recordVideo: { dir: raw, size: entry.viewport },
    });
    const page = await ctx.newPage();
    await page.goto(entry.url, { waitUntil: "networkidle" });
    const driver = await driverFor(entry);
    if (driver) await driver(page, ctx);
    else await runSteps(page, entry.steps);
    await page.waitForTimeout(entry.settle ?? 800);
    const video = page.video();
    await ctx.close(); // finalizes the recording
    const rawFile = join(OUT_TMP, `${entry.id}-raw.webm`);
    await video.saveAs(rawFile); // saveAs, not path() — required with connect()
    // Post-process with a real ffmpeg (Playwright's bundled one can't filter).
    const files = {};
    for (const out of entry.out) {
      if (out.endsWith(".webm")) {
        files.webm ??= join(OUT_TMP, `${entry.id}.webm`);
      } else if (out.endsWith(".mp4")) {
        files.mp4 ??= join(OUT_TMP, `${entry.id}.mp4`);
      }
    }
    // entry.trim (seconds): cut the delivery at a manifest-declared point —
    // for endings the raw recording must not ship (see the entry's note).
    // entry.freeze (seconds): hold the final kept frame so a trimmed ending
    // still breathes (tpad clones the last real frame — nothing synthetic).
    const cut = entry.trim ? ["-t", String(entry.trim)] : [];
    const pad = entry.freeze
      ? ["-vf", `tpad=stop_mode=clone:stop_duration=${entry.freeze}`]
      : [];
    // `cut` goes BEFORE -i (an input option): trim what's decoded, so the
    // freeze pad extends the kept ending instead of being cut off itself.
    if (files.webm)
      ffmpeg([...cut, "-i", rawFile, ...pad, "-c:v", "libvpx-vp9", "-b:v", "0", "-crf", "38", "-an", files.webm]);
    if (files.mp4)
      ffmpeg([...cut, "-i", rawFile, ...pad, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "26", "-preset", "slow", "-an", "-movflags", "+faststart", files.mp4]);
    if (entry.poster) {
      // Not literally frame 0 — recording starts before the first paint, so
      // frame 0 is a blank page. Seek past the paint to the settled opening
      // state instead.
      files.poster = join(OUT_TMP, `${entry.id}-poster.png`);
      ffmpeg(["-ss", "1.2", "-i", rawFile, "-frames:v", "1", files.poster]);
    }
    return files;
  } finally {
    await browser.close();
  }
}

// ── budgets & delivery ───────────────────────────────────────────────────

function checkBudget(entry, file, { isPoster = false } = {}) {
  const kind = isPoster ? "still" : entry.kind;
  const b = BUDGET[kind];
  const size = statSync(file).size;
  if (size > b.bytes)
    throw new Error(
      `budget: ${entry.id} → ${file} is ${(size / 1024).toFixed(0)} KB ` +
        `(limit ${(b.bytes / 1024).toFixed(0)} KB)`,
    );
  if (b.seconds && /\.(webm|mp4)$/.test(file)) {
    const s = ffprobeSeconds(file);
    if (s > b.seconds)
      throw new Error(`budget: ${entry.id} → ${file} runs ${s.toFixed(1)} s (limit ${b.seconds} s)`);
  }
  if (b.seconds && file.endsWith(".cast")) {
    const lines = readFileSync(file, "utf8").trim().split("\n");
    const last = JSON.parse(lines[lines.length - 1]);
    if (last[0] > b.seconds)
      throw new Error(`budget: ${entry.id} → cast runs ${last[0]} s (limit ${b.seconds} s)`);
  }
}

function deliver(entry, tmpByExt) {
  for (const out of entry.out) {
    const abs = join(ROOT, out);
    const ext = out.slice(out.lastIndexOf("."));
    const src =
      typeof tmpByExt === "string"
        ? tmpByExt
        : out.includes("poster")
          ? tmpByExt.poster
          : tmpByExt[ext.slice(1)];
    if (!src) throw new Error(`${entry.id}: no produced file for ${out}`);
    mkdirSync(dirname(abs), { recursive: true });
    copyFileSync(src, abs);
  }
  if (entry.poster && typeof tmpByExt !== "string") {
    const abs = join(ROOT, entry.poster);
    mkdirSync(dirname(abs), { recursive: true });
    copyFileSync(tmpByExt.poster, abs);
    checkBudget(entry, abs, { isPoster: true });
  }
  for (const out of entry.out) checkBudget(entry, join(ROOT, out));
}

function verifyOnly(entry) {
  for (const out of entry.out) {
    const abs = join(ROOT, out);
    if (!existsSync(abs))
      throw new Error(
        `${entry.id} (${entry.kind}) not on disk: ${out} — see manifest note`,
      );
    checkBudget(entry, abs);
  }
}

// ── run ──────────────────────────────────────────────────────────────────

const results = { ok: [], skipped: [], failed: [] };
for (const entry of selected) {
  if (entry.blockedOn) {
    results.skipped.push(`${entry.id} (blocked on ${entry.blockedOn})`);
    continue;
  }
  try {
    if (entry.kind === "still") deliver(entry, await captureStill(entry));
    else if (entry.kind === "clip") deliver(entry, await captureClip(entry));
    else verifyOnly(entry); // cast | manual | static
    results.ok.push(entry.id);
    console.log(`✓ ${entry.id}`);
  } catch (err) {
    results.failed.push(`${entry.id}: ${err.message}`);
    console.error(`✗ ${entry.id}: ${err.message}`);
  }
}

console.log(
  `\ncapture: ${results.ok.length} ok, ${results.skipped.length} skipped, ${results.failed.length} failed`,
);
for (const s of results.skipped) console.log(`  – ${s}`);
if (results.failed.length) {
  for (const f of results.failed) console.log(`  ✗ ${f}`);
  process.exit(1);
}
