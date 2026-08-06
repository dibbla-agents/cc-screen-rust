// ANSI → PNG renderer for the ccs terminal stills (proposal 0061 B5).
//
//   node ansi2png.mjs <dump.ans> <out.png> [--font-size 6.7] [--line-height 10]
//
// Input is a `tmux capture-pane -e -p` dump (plain text + SGR sequences).
// The script converts it to a self-contained HTML page (spans over a dark
// terminal background, monospace font) and screenshots the <pre> element at
// deviceScaleFactor 2 with Playwright. Set CAPTURE_WS_ENDPOINT to use the
// real-Chrome browser server (same contract as capture.mjs); falls back to a
// local headless chromium.

import { readFileSync } from "node:fs";
import pw from "playwright";

const [, , inFile, outFile, ...rest] = process.argv;
if (!inFile || !outFile) {
  console.error("usage: node ansi2png.mjs <dump.ans> <out.png> [--font-size N] [--line-height N]");
  process.exit(2);
}
function opt(name, dflt) {
  const i = rest.indexOf(name);
  return i >= 0 ? parseFloat(rest[i + 1]) : dflt;
}
const FONT_SIZE = opt("--font-size", 6.7); // CSS px; ×2 dpr ≈ a 13.4px raster
const LINE_H = opt("--line-height", 10); // CSS px per row
const COLS = opt("--cols", 0); // pad every row to N columns (0 = no padding)

// ── palette ────────────────────────────────────────────────────────────────
const DEFAULT_FG = "#c9d1d9";
const DEFAULT_BG = "#121b24"; // terminal background (ccs draws its bars darker)
const ANSI16 = [
  "#1c252e", "#f38ba8", "#7ad3a2", "#e5c07b", "#7aa2f7", "#c678dd", "#5fd0d8", "#c9d1d9",
  "#5c6773", "#ff7a93", "#8ff0b0", "#f7d774", "#8caaee", "#d693e8", "#7adfe2", "#eef2f6",
];
function xterm256(n) {
  if (n < 16) return ANSI16[n];
  if (n < 232) {
    const c = n - 16;
    const lv = (x) => (x === 0 ? 0 : 55 + x * 40);
    const r = lv(Math.floor(c / 36)), g = lv(Math.floor((c % 36) / 6)), b = lv(c % 6);
    return `rgb(${r},${g},${b})`;
  }
  const v = 8 + (n - 232) * 10;
  return `rgb(${v},${v},${v})`;
}

// ── SGR state machine ──────────────────────────────────────────────────────
const fresh = () => ({ fg: null, bg: null, bold: false, dim: false, italic: false, ul: false, rev: false });
let st = fresh();
function applySgr(params) {
  const p = params.length ? params : [0];
  for (let i = 0; i < p.length; i++) {
    const n = p[i];
    if (n === 0) st = fresh();
    else if (n === 1) st.bold = true;
    else if (n === 2) st.dim = true;
    else if (n === 3) st.italic = true;
    else if (n === 4) st.ul = true;
    else if (n === 7) st.rev = true;
    else if (n === 22) { st.bold = false; st.dim = false; }
    else if (n === 23) st.italic = false;
    else if (n === 24) st.ul = false;
    else if (n === 27) st.rev = false;
    else if (n >= 30 && n <= 37) st.fg = ANSI16[n - 30];
    else if (n === 39) st.fg = null;
    else if (n >= 40 && n <= 47) st.bg = ANSI16[n - 40];
    else if (n === 49) st.bg = null;
    else if (n >= 90 && n <= 97) st.fg = ANSI16[n - 90 + 8];
    else if (n >= 100 && n <= 107) st.bg = ANSI16[n - 100 + 8];
    else if (n === 38 || n === 48) {
      const set = (v) => (n === 38 ? (st.fg = v) : (st.bg = v));
      if (p[i + 1] === 5) { set(xterm256(p[i + 2] ?? 0)); i += 2; }
      else if (p[i + 1] === 2) { set(`rgb(${p[i + 2] ?? 0},${p[i + 3] ?? 0},${p[i + 4] ?? 0})`); i += 4; }
    }
  }
}
function styleCss() {
  let fg = st.fg ?? DEFAULT_FG, bg = st.bg; // null bg = page background
  if (st.rev) [fg, bg] = [bg ?? DEFAULT_BG, fg];
  const s = [`color:${fg}`];
  if (bg) s.push(`background:${bg}`);
  if (st.bold) s.push("font-weight:700");
  if (st.dim) s.push("opacity:.55");
  if (st.italic) s.push("font-style:italic");
  if (st.ul) s.push("text-decoration:underline");
  return s.join(";");
}
const esc = (t) => t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// ── ANSI text → HTML ───────────────────────────────────────────────────────
const raw = readFileSync(inFile, "utf8").replace(/\n+$/, "");
let html = "";
for (const line of raw.split("\n")) {
  let out = "", run = "", runStyle = styleCss();
  const flush = () => { if (run) out += `<span style="${runStyle}">${esc(run)}</span>`; run = ""; };
  let i = 0;
  while (i < line.length) {
    if (line[i] === "\x1b") {
      const m = /^\x1b\[([0-9;:]*)m/.exec(line.slice(i));
      if (m) { flush(); applySgr(m[1].split(/[;:]/).filter((x) => x !== "").map(Number)); runStyle = styleCss(); i += m[0].length; continue; }
      const skip = /^\x1b(\[[0-9;?]*[A-Za-z]|\][^\x07]*\x07|[()][AB0])/.exec(line.slice(i));
      i += skip ? skip[0].length : 1; // drop non-SGR escapes
      continue;
    }
    run += line[i++];
  }
  flush();
  if (COLS > 0) {
    // Pad with default-style spaces so the background spans the full width
    // (tmux strips trailing blanks). Width ≈ chars minus escape sequences —
    // good enough since padded cells are plain.
    const width = [...line.replace(/\x1b\[[0-9;:?]*[A-Za-z]/g, "")].length;
    if (width < COLS) out += `<span style="color:${DEFAULT_FG}">${" ".repeat(COLS - width)}</span>`;
  }
  html += out + "\n";
}

const page_html = `<!doctype html><meta charset="utf-8"><style>
  html,body{margin:0;padding:0;background:${DEFAULT_BG};}
  pre{margin:0;display:inline-block;background:${DEFAULT_BG};
      font:${FONT_SIZE}px/${LINE_H}px "Menlo","SF Mono","DejaVu Sans Mono",monospace;
      white-space:pre;color:${DEFAULT_FG};
      -webkit-font-smoothing:antialiased;}
</style><pre>${html}</pre>`;

// ── screenshot ─────────────────────────────────────────────────────────────
const ws = process.env.CAPTURE_WS_ENDPOINT;
const browser = ws
  ? await pw.chromium.connect(ws)
  : await pw.chromium.launch({ headless: true, args: ["--no-sandbox"] });
const page = await browser.newPage({ viewport: { width: 1000, height: 700 }, deviceScaleFactor: 2 });
await page.setContent(page_html);
await page.evaluate(() => document.fonts.ready);
const el = page.locator("pre");
await el.screenshot({ path: outFile });
await browser.close();
console.log(`wrote ${outFile}`);
