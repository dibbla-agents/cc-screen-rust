// Headless end-to-end smoke test of the built PWA in a phone viewport.
// Loads the page, switches to a throwaway session, exercises the arrow/Enter
// control keys and the compose sheet, and fails on any console/page error.
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import { writeFileSync, readFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { join, relative, sep } from "node:path";

const base = process.env.BASE || "http://127.0.0.1:8840";
const exe = process.env.CHROME;
// Short name of the throwaway session to drive (never a live one).
const session = process.env.SESSION || "smoketest";
// Full tmux name. The swipe assertion no longer uses it (proposal 0031 repaired
// that one); the SIGINT capture-pane check still does — repairing the rest of
// this harness's tmux assumptions stays [0066] Part E's deferred job.
const tmuxSession = process.env.TMUX_SESSION || `claude-${session}`;

// GL is ENABLED here (software rasterizer): the terminals adopt the WebGL
// renderer (proposal 0068 Part D), so every terminal step below — selection,
// swipe scroll, find — is exercised on the renderer real users get. The DOM
// fallback gets its own launch in domFallbackPass().
const GL_ARGS = ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"];
const browser = await chromium.launch({
  executablePath: exe,
  headless: true,
  args: ["--no-sandbox", ...GL_ARGS],
});
const ctx = await browser.newContext({
  viewport: { width: 390, height: 844 },
  deviceScaleFactor: 2,
  isMobile: true,
  hasTouch: true,
});
const page = await ctx.newPage();

// A throwaway markdown file in the share folder for the editor flow. The Files
// sheet auto-expands the share section, so it's reachable without navigation.
const shareDir = process.env.CCWEB_SHARE_DIR || join(homedir(), "cc-share");
mkdirSync(shareDir, { recursive: true });
const editFile = join(shareDir, "ccwebsmoke_edit.md");
const newMdName = "ccwebsmoke_new.md";
const newMdPath = join(shareDir, newMdName);
writeFileSync(editFile, "# Smoke Heading\n\nHello **world**.\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n");

// A throwaway one-page PDF for the editor's pdf.js viewer flow. Built by hand
// with correct xref offsets so pdf.js parses it cleanly (no recovery warnings).
const pdfName = "ccwebsmoke_doc.pdf";
const pdfPath = join(shareDir, pdfName);
function makeMinimalPdf() {
  const content = "BT /F1 24 Tf 60 120 Td (Smoke PDF) Tj ET";
  const objs = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let body = "%PDF-1.4\n";
  const offsets = [];
  objs.forEach((o, i) => {
    offsets.push(body.length);
    body += `${i + 1} 0 obj\n${o}\nendobj\n`;
  });
  const xref = body.length;
  body += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n`;
  offsets.forEach((off) => {
    body += `${String(off).padStart(10, "0")} 00000 n \n`;
  });
  body += `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(body, "latin1");
}
writeFileSync(pdfPath, makeMinimalPdf());

// Phone upload target: the footer Upload button posts into the session's cwd
// (the UploadSheet's default destination = project root). Resolve that cwd up
// front so we can assert the file landed and clean it up afterwards.
const uploadName = "ccwebsmoke_upload.txt";
let uploadCwd = "";
try {
  uploadCwd = execFileSync("tmux", [
    "display-message", "-p", "-t", tmuxSession, "#{pane_current_path}",
  ]).toString().trim();
} catch {}
const uploadedPath = uploadCwd ? join(uploadCwd, uploadName) : "";

const errors = [];
const api = [];
page.on("console", (m) => {
  if (m.type() === "error") errors.push("console: " + m.text());
});
page.on("pageerror", (e) => errors.push("pageerror: " + e.message));
page.on("response", (r) => {
  try {
    const u = new URL(r.url());
    if (u.pathname.startsWith("/api/")) api.push(`${r.request().method()} ${u.pathname}${u.search} -> ${r.status()}`);
  } catch {}
});
page.on("websocket", (ws) => api.push(`WS ${new URL(ws.url()).search}`));

function fail(msg) {
  console.error("SMOKE FAIL:", msg);
  console.error("API calls:\n  " + api.join("\n  "));
  if (errors.length) console.error("JS errors:\n  " + errors.join("\n  "));
  process.exitCode = 1;
}

// desktopEditorPass exercises the desktop-only entry points in a wide,
// fine-pointer context: mount a session in a pane, open the editor via the
// per-pane top-right button, and pick a file from the left tree. (The phone
// pass above covers the Files-sheet tap, edit/save, reading view, and new-file.)
async function desktopEditorPass() {
  const dctx = await browser.newContext({ viewport: { width: 1280, height: 820 } });
  const dpage = await dctx.newPage();
  const dapi = [];
  const derrs = [];
  dpage.on("console", (m) => {
    if (m.type() === "error") derrs.push("console: " + m.text());
  });
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  dpage.on("response", (r) => {
    try {
      const u = new URL(r.url());
      if (u.pathname.startsWith("/api/")) dapi.push(`${r.request().method()} ${u.pathname} -> ${r.status()}`);
    } catch {}
  });
  try {
    await dpage.goto(base, { waitUntil: "networkidle" });
    // Desktop shows the real switcher in the empty pane ([0026]) — mount the
    // throwaway session so the pane gets its top-right chrome (the editor
    // button). Wait on the session's own row, not on placeholder copy: the old
    // `getByText("Empty pane")` matched nothing (that string lives only in code
    // comments) and burned its 8 s timeout on every run.
    await dpage.getByRole("button", { name: new RegExp(session) }).first().click({ timeout: 15000 });
    await dpage.waitForSelector('[data-conn="open"]', { timeout: 10000 });
    // The pane chrome auto-hides; hover to reveal it, then click the per-pane
    // "Open file editor" button (the entry point we're testing).
    await dpage.locator(".xterm").first().hover();
    await dpage.getByRole("button", { name: "Open file browser / editor" }).click({ timeout: 5000 });
    // The left tree renders its sections (project first + auto-expanded; Home
    // and Share below, collapsed). The throwaway file lives in the share
    // folder, so expand that section before picking it.
    await dpage.getByText("Share folder", { exact: true }).waitFor({ timeout: 8000 });
    await dpage.getByText("Share folder", { exact: true }).click();
    // Pick the markdown file from the tree → it loads with live preview.
    await dpage.getByRole("button", { name: "ccwebsmoke_edit.md", exact: true }).click({ timeout: 8000 });
    await dpage.waitForSelector(".cm-md-h1", { timeout: 8000 });
    // The GFM table renders as a real <table> widget (live-preview, off-cursor).
    await dpage.waitForSelector(".cm-md-table", { timeout: 8000 });
    // PDF in the same (share) section → the singleton overlay swaps to the
    // pdf.js viewer; assert a page canvas rasterises on desktop too.
    await dpage.getByRole("button", { name: "ccwebsmoke_doc.pdf", exact: true }).click({ timeout: 8000 });
    let dpdfRendered = false;
    try {
      await dpage.waitForFunction(
        () => {
          const c = document.querySelector("canvas.cc-pdf-canvas");
          return !!c && c.width > 0 && c.height > 0;
        },
        { timeout: 12000 }
      );
      dpdfRendered = true;
    } catch {}
    await dpage.keyboard.press("Escape");
    if (derrs.length) {
      fail("desktop editor JS errors: " + derrs.join("; "));
    } else if (!dapi.some((a) => a.startsWith("GET /api/file/read"))) {
      fail("desktop editor never read a file");
    } else if (!dpdfRendered) {
      fail("desktop PDF viewer didn't rasterise a page canvas");
    }
  } catch (e) {
    console.error("desktop API calls:\n  " + dapi.join("\n  "));
    if (derrs.length) console.error("desktop JS errors:\n  " + derrs.join("\n  "));
    fail("desktop editor pass: " + e.message);
  } finally {
    await dctx.close();
  }
}

// Mount the throwaway session in a desktop pane and return the page. Shared by
// the desktop passes below (the empty pane renders the real switcher, so
// picking the session is a click on its row).
async function mountDesktopSession(dpage) {
  await dpage.goto(base, { waitUntil: "networkidle" });
  // The empty pane renders the real switcher, so the session's own row is the
  // thing to wait for (its copy is stable; the pane's placeholder text is not).
  await dpage.getByRole("button", { name: new RegExp(session) }).first().click({ timeout: 15000 });
  await dpage.waitForSelector('[data-conn="open"]', { timeout: 10000 });
}

// ── Proposal 0068 ─────────────────────────────────────────────────────────────
// The renderer swap (Part D), the idle-quiescence budget (Parts A/B), the
// closed-drawer row gating (Part B) and terminal find (Part E), in one desktop
// pass on the GL-enabled browser.
async function idleRendererPass() {
  const dctx = await browser.newContext({ viewport: { width: 1280, height: 820 } });
  const dpage = await dctx.newPage();
  const derrs = [];
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  try {
    await mountDesktopSession(dpage);

    // 1) The WebGL renderer is live (GL_ARGS above give headless Chrome a
    //    software GL implementation).
    const renderer = await dpage.evaluate(() => window.__ccRenderer);
    if (renderer !== "webgl") {
      fail(`expected the WebGL renderer, got ${renderer ?? "undefined"}`);
      return;
    }

    // 2) Idle quiescence. With no input and no output, an idle tab must not
    //    repaint continuously: no infinite CSS animation anywhere in the DOM,
    //    and style/layout counts that barely move across a 10s window (the poll
    //    ticks are the only expected work). Before proposal 0068 the blinking
    //    cursor alone drove these into the hundreds.
    const metrics = async () => {
      const cdp = await dpage.context().newCDPSession(dpage);
      const { metrics: m } = await cdp.send("Performance.getMetrics");
      const at = (n) => m.find((x) => x.name === n)?.value ?? 0;
      return { style: at("RecalcStyleCount"), layout: at("LayoutCount") };
    };
    await dpage.waitForTimeout(1500); // let the attach settle
    const before = await metrics();
    await dpage.waitForTimeout(10_000);
    const after = await metrics();
    const dStyle = after.style - before.style;
    const dLayout = after.layout - before.layout;
    const infinite = await dpage.evaluate(() =>
      document
        .getAnimations()
        .filter((a) => a.effect?.getTiming?.().iterations === Infinity)
        .map((a) => a.effect?.target?.className ?? "?")
    );
    if (infinite.length) {
      fail(`idle DOM has infinite CSS animations: ${infinite.join(", ")}`);
      return;
    }
    if (dStyle > 60 || dLayout > 60) {
      fail(`idle tab is not quiescent over 10s: +${dStyle} restyles, +${dLayout} layouts`);
      return;
    }

    // 3) A closed drawer renders no session rows (the root and its slide stay
    //    mounted; only the list body unmounts).
    const closedRows = await dpage.locator('[data-drawer="closed"] [data-session-row]').count();
    if (closedRows !== 0) {
      fail(`closed drawer still renders ${closedRows} session rows`);
      return;
    }
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press("s");
    await dpage.waitForSelector('[data-drawer="open"] [data-session-row]', { timeout: 5000 });
    await dpage.keyboard.press("Escape");

    // 4) Terminal find (⌃B /) — the replacement for browser find-in-page over
    //    terminal output. Type a token into the terminal, search for it, and
    //    assert the addon selected the match.
    const token = "SMOKEFIND";
    await dpage.locator(".xterm").first().click({ position: { x: 50, y: 60 } });
    await dpage.keyboard.type(token);
    await dpage.waitForTimeout(400);
    // Type the chord the way a Swedish keyboard does — "/" is Shift+7, so the
    // browser delivers a bare "Shift" keydown between the prefix and the chord
    // key. That used to cancel the prefix (it looked like an unrecognised
    // chord), which broke ⌃B / on every layout where "/" needs a modifier —
    // and ⌃B ⇧S on all of them.
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.down("Shift");
    await dpage.keyboard.press("/");
    await dpage.keyboard.up("Shift");
    await dpage.waitForSelector('[data-testid="term-find"]', { timeout: 5000 });
    await dpage.keyboard.type(token);
    await dpage.waitForTimeout(400);
    const selection = await dpage.evaluate(() => window.__ccTerm?.getSelection?.() ?? "");
    await dpage.keyboard.press("Escape");
    const findGone = (await dpage.locator('[data-testid="term-find"]').count()) === 0;
    // Clear the typed token off the agent's prompt line.
    await dpage.keyboard.press("Control+c");
    if (!selection.includes(token)) {
      fail(`⌃B / did not select the match (selection: ${JSON.stringify(selection)})`);
      return;
    }
    if (!findGone) {
      fail("Esc did not close the terminal find bar");
      return;
    }
    if (derrs.length) fail("idle/renderer pass JS errors: " + derrs.join("; "));
  } catch (e) {
    fail("idle/renderer pass: " + e.message);
  } finally {
    await dctx.close();
  }
}

// ── Proposal 0078 ─────────────────────────────────────────────────────────────
// The `Recent` section: work in one session, switch to another, and the drawer
// must lead with the one you just left — and never with the one you are in.
// Needs two sessions on the box; with fewer, the section is suppressed by
// design (A7) and the pass logs a skip rather than failing.
async function recentSectionPass() {
  const dctx = await browser.newContext({ viewport: { width: 1280, height: 820 } });
  const dpage = await dctx.newPage();
  const derrs = [];
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  try {
    await dpage.goto(base, { waitUntil: "networkidle" });
    const rows = dpage.locator("[data-session-row]");
    await rows.first().waitFor({ timeout: 15000 });
    const names = await rows.evaluateAll((els) =>
      els.map((e) => e.querySelector("span.font-semibold")?.textContent?.trim() ?? "")
    );
    const uniq = [...new Set(names.filter(Boolean))];
    if (uniq.length < 2) {
      console.log(`SKIP recent-section pass: needs 2 sessions, saw ${uniq.length}`);
      return;
    }
    const [first, second] = uniq;

    // Work in `first` for longer than the 1s dwell gate, then switch to `second`.
    await dpage.getByRole("button", { name: new RegExp(first) }).first().click({ timeout: 15000 });
    await dpage.waitForSelector('[data-conn="open"]', { timeout: 10000 });
    await dpage.waitForTimeout(1600);
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press("s");
    await dpage.waitForSelector('[data-drawer="open"] [data-session-row]', { timeout: 5000 });
    await dpage.getByRole("button", { name: new RegExp(second) }).first().click({ timeout: 10000 });
    await dpage.waitForTimeout(1600);

    // Reopen: the section leads with `first`, and excludes the mounted `second`.
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press("s");
    await dpage.waitForSelector('[data-drawer="open"] [data-recent-section]', { timeout: 5000 });
    const section = await dpage
      .locator('[data-drawer="open"] [data-session-row][data-recent-section]')
      .evaluateAll((els) =>
        els.map((e) => e.querySelector("span.font-semibold")?.textContent?.trim() ?? "")
      );
    // The 0078 addendum: the cursor is parked on that first row, so ⏎ alone
    // goes back to the session you were just in — two keystrokes total. Proof
    // is the round trip: reopening the drawer now leads with `second`, because
    // `first` has become the mounted one. (The drawer is still open here.)
    await dpage.keyboard.press("Enter");
    await dpage.waitForTimeout(1800);
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press("s");
    await dpage.waitForSelector('[data-drawer="open"] [data-recent-section]', { timeout: 5000 });
    const back = await dpage
      .locator('[data-drawer="open"] [data-session-row][data-recent-section]')
      .evaluateAll((els) =>
        els.map((e) => e.querySelector("span.font-semibold")?.textContent?.trim() ?? "")
      );
    await dpage.keyboard.press("Escape");
    if (back[0] !== second) {
      fail(`⏎ on the parked cursor didn't switch back (section now leads with ${JSON.stringify(back[0])})`);
      return;
    }
    if (section[0] !== first) {
      fail(`Recent leads with ${JSON.stringify(section[0])}, expected ${JSON.stringify(first)}`);
      return;
    }
    if (section.includes(second)) {
      fail(`the mounted session ${JSON.stringify(second)} must not be in Recent`);
      return;
    }
    if (derrs.length) fail("recent-section pass JS errors: " + derrs.join("; "));
  } catch (e) {
    fail("recent-section pass: " + e.message);
  } finally {
    await dctx.close();
  }
}

// ── Proposal 0081 ─────────────────────────────────────────────────────────────
// The ⌃B pane keymap, which had no coverage at all — which is how the arrow
// wrap could be dead in two layouts, the empty pane could swallow the whole
// prefix, and every pane switch could pop a rename box, all unreported.
//
// Every assertion here reads [data-pane-active] (Part E); the whole pass fails
// on the pre-fix build, first at the focused-input check (Part H), then at the
// stuck arrow (Part A) and the swallowed prefix (Part C).
// ── Proposal 0083 Parts A/B ───────────────────────────────────────────────────
// The whole reason the proposal exists: a URL you can bookmark opens the file
// with ZERO clicks, on desktop and on a phone. Also covered here: the folder
// form, the heal-don't-latch path for a link whose file is gone, and that the
// address bar mints the link itself (⌘D is the "create bookmark" gesture).
async function deepLinkPass() {
  // Links are home-relative by construction (fileLink.ts), so the seeded file
  // has to be under $HOME for there to be a URL at all.
  const rel = relative(homedir(), editFile).split(sep).join("/");
  if (rel.startsWith("..")) {
    console.log("SKIP deep-link pass: CCWEB_SHARE_DIR is outside $HOME, so the file has no link form");
    return;
  }
  const fileUrl = "/file/-/" + rel.split("/").map(encodeURIComponent).join("/");
  const dirUrl = "/file/-/" + relative(homedir(), shareDir).split(sep).map(encodeURIComponent).join("/") + "/";

  const dctx = await browser.newContext({
    viewport: { width: 1280, height: 820 },
    permissions: ["clipboard-read", "clipboard-write"],
  });
  const dpage = await dctx.newPage();
  const derrs = [];
  dpage.on("console", (m) => {
    // The healing step below deliberately requests a file that isn't there, and
    // Chrome logs every 4xx as a console error. That one is the assertion, not
    // a defect — everything else still fails the pass.
    if (m.type() === "error" && !m.text().includes("Failed to load resource")) {
      derrs.push("console: " + m.text());
    }
  });
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  try {
    // ── Desktop: one navigation, zero clicks ────────────────────────────────
    await dpage.goto(base + fileUrl, { waitUntil: "domcontentloaded" });
    await dpage.waitForSelector(".cm-content", { timeout: 15000 });
    const body = await dpage.locator(".cm-content").first().innerText();
    if (!body.includes("Smoke Heading")) {
      fail(`deep link opened the editor but not the file (got: ${body.slice(0, 80)})`);
      return;
    }
    // The URL is NOT stripped on success — that is the bookmark.
    let path = await dpage.evaluate(() => location.pathname);
    if (path !== fileUrl) {
      fail(`deep link rewrote the URL to ${path}, expected ${fileUrl}`);
      return;
    }

    // Editing through a deep-linked buffer is the ordinary editor.
    await dpage.locator(".cm-content").first().click();
    await dpage.keyboard.press("End");
    await dpage.keyboard.type(" EDITED-BY-LINK");
    await dpage.waitForTimeout(1200); // autosave debounce
    if (!readFileSync(editFile, "utf8").includes("EDITED-BY-LINK")) {
      fail("a deep-linked file opened read-only — Part A is the app, authenticated as you");
      return;
    }

    // ── Copy link mints exactly the URL we arrived on ───────────────────────
    await dpage.getByRole("button", { name: "Copy link to this file" }).click({ timeout: 8000 });
    const copied = await dpage.evaluate(() => navigator.clipboard.readText());
    if (copied !== base.replace(/\/+$/, "") + fileUrl) {
      fail(`Copy link produced ${copied}, expected ${base + fileUrl}`);
      return;
    }

    // ── The folder form opens the tree, not a buffer ────────────────────────
    await dpage.goto(base + dirUrl, { waitUntil: "domcontentloaded" });
    await dpage.getByRole("button", { name: "ccwebsmoke_edit.md", exact: true }).first().waitFor({ timeout: 15000 });
    if ((await dpage.locator(".cm-content").count()) > 0) {
      fail("the folder form opened a file buffer");
      return;
    }

    // ── Heal, don't latch: a link to a file that isn't there ────────────────
    await dpage.goto(base + "/file/-/" + rel.replace(/[^/]+$/, "ccwebsmoke_vanished.md"), {
      waitUntil: "domcontentloaded",
    });
    await dpage.getByText(/isn’t here anymore|isn't here anymore/).waitFor({ timeout: 15000 });
    if ((await dpage.locator(".cm-content").count()) > 0) {
      fail("a stale deep link opened a buffer instead of healing to the tree");
      return;
    }

    // ── Phone: the device this proposal exists for. Zero taps. ──────────────
    const pctx = await browser.newContext({
      viewport: { width: 390, height: 844 },
      deviceScaleFactor: 2,
      isMobile: true,
      hasTouch: true,
    });
    const ppage = await pctx.newPage();
    try {
      await ppage.goto(base + fileUrl, { waitUntil: "domcontentloaded" });
      await ppage.waitForSelector(".cm-content", { timeout: 15000 });
      const ptext = await ppage.locator(".cm-content").first().innerText();
      if (!ptext.includes("Smoke Heading")) {
        fail("phone deep link didn't open the file with zero taps");
        return;
      }
      // The session drawer must not have covered it (0083 Mobile/touch).
      if (await ppage.getByPlaceholder(/search sessions/i).isVisible().catch(() => false)) {
        fail("the session drawer opened over a phone deep link");
        return;
      }
    } finally {
      await pctx.close();
    }

    if (derrs.length) fail("deep-link JS errors: " + derrs.join("; "));
  } catch (e) {
    if (derrs.length) console.error("deep-link JS errors:\n  " + derrs.join("\n  "));
    fail("deep-link pass: " + e.message);
  } finally {
    await dctx.close();
  }
}

async function gridKeyboardPass() {
  const dctx = await browser.newContext({ viewport: { width: 1280, height: 820 } });
  const dpage = await dctx.newPage();
  const derrs = [];
  // The "any JS error fails the suite" listener at the bottom is the PHONE
  // page's; a desktop pass has to bring its own.
  dpage.on("console", (m) => {
    if (m.type() === "error") derrs.push("console: " + m.text());
  });
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  // Part H's network half: switching panes must never write a display label.
  let labelPosts = 0;
  dpage.on("request", (r) => {
    if (r.method() === "POST" && new URL(r.url()).pathname === "/api/session/label") labelPosts++;
  });

  // Which pane index currently carries the focus ring, and what has the caret.
  const activePane = () =>
    dpage.evaluate(() => document.querySelector("[data-pane-active]")?.getAttribute("data-pane") ?? null);
  const chord = async (k) => {
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press(k);
    await dpage.waitForTimeout(120);
  };
  // Part H, asserted after EVERY switch: a pane switch moves focus into the
  // pane and nowhere else. On the pre-fix build the identity bar's rename input
  // has the caret here — and, being an <input>, it also kills the next chord.
  // An *empty* pane's switcher filter taking the caret is the one legitimate
  // case ([0026]), so the check is "not a rename field", not "not an input".
  const noCaretInField = async (what) => {
    const bad = await dpage.evaluate(() => {
      const el = document.activeElement;
      if (!el || el.tagName !== "INPUT") return null;
      if (el.hasAttribute("data-pane-filter")) return null; // the empty pane's switcher
      return el.getAttribute("aria-label") || "an <input>";
    });
    if (bad) {
      fail(`${what} put the caret in ${bad} — a pane switch must not open the rename box (0081 H)`);
      return false;
    }
    return true;
  };
  // Layouts are picked by accessible name, never by digit: the palette's digits
  // are ORDER positions, not layout ids (Stacked is layout 5 but digit 2), so a
  // digit-driven test silently exercises the wrong layout.
  const pickLayout = async (name) => {
    await chord("l");
    await dpage.waitForSelector('[role="dialog"][aria-label="Pick a layout"]', { timeout: 5000 });
    // No trailing Enter — onClick applies and closes; a stray Enter would fall
    // through to the terminal.
    await dpage.getByRole("button", { name, exact: true }).click({ timeout: 5000 });
    await dpage.waitForSelector('[role="dialog"][aria-label="Pick a layout"]', {
      state: "detached",
      timeout: 5000,
    });
  };
  const paneCount = () => dpage.locator("[data-pane]").count();

  try {
    await mountDesktopSession(dpage);

    // ── Stacked: layout id 5, TWO panes. `(cur+1+lay)%lay` used to compute 2
    // here, which setActive clamped straight back onto the last pane.
    await pickLayout("Stacked");
    if ((await paneCount()) !== 2) {
      fail(`Stacked should render 2 panes, saw ${await paneCount()}`);
      return;
    }
    await chord("2");
    if (!(await noCaretInField("⌃B 2"))) return;
    if ((await activePane()) !== "1") {
      fail(`⌃B 2 did not focus pane 2 (active=${await activePane()})`);
      return;
    }
    await chord("ArrowRight");
    if (!(await noCaretInField("⌃B →"))) return;
    if ((await activePane()) !== "0") {
      fail(`⌃B → did not wrap from the last pane in Stacked (active=${await activePane()}) — 0081 Part A`);
      return;
    }

    // ── The empty pane (pane 2 holds nothing) must not eat the prefix. It is
    // focused right now via ⌃B 2 below; ⌃B 1 has to get us back out.
    await chord("2");
    await dpage.waitForSelector("[data-pane-filter]", { timeout: 5000 });
    await chord("1");
    if ((await activePane()) !== "0") {
      fail(`⌃B 1 from a focused EMPTY pane did nothing — the switcher filter swallowed the prefix (0081 Part C)`);
      return;
    }
    const filterText = await dpage
      .locator("[data-pane-filter]")
      .first()
      .inputValue()
      .catch(() => "");
    if (filterText.includes("1")) {
      fail(`the chord key leaked into the empty pane's filter (${JSON.stringify(filterText)})`);
      return;
    }
    // Part E: an empty pane shows its number, which is the argument to ⌃B <n>.
    const emptyNumber = await dpage
      .locator('[data-pane="1"] span.pointer-events-none.font-mono')
      .first()
      .textContent()
      .catch(() => null);
    if (emptyNumber?.trim() !== "2") {
      fail(`the empty pane does not render its 1-based number (saw ${JSON.stringify(emptyNumber)}) — 0081 Part E`);
      return;
    }

    // ── Right-tall L: layout id 6, THREE panes — the other stuck-arrow case.
    await pickLayout("Right-tall L");
    if ((await paneCount()) !== 3) {
      fail(`Right-tall L should render 3 panes, saw ${await paneCount()}`);
      return;
    }
    await chord("3");
    if ((await activePane()) !== "2") {
      fail(`⌃B 3 did not focus pane 3 (active=${await activePane()})`);
      return;
    }
    await chord("ArrowRight");
    if ((await activePane()) !== "0") {
      fail(`⌃B → did not wrap from the last pane in Right-tall L (active=${await activePane()}) — 0081 Part A`);
      return;
    }
    if (!(await noCaretInField("⌃B → (layout 6)"))) return;

    // ── ⌃B ; — last-pane. We are on 0, having come from 2.
    await chord(";");
    if ((await activePane()) !== "2") {
      fail(`⌃B ; did not return to the pane we came from (active=${await activePane()}) — 0081 Part D`);
      return;
    }
    // A bare Shift keydown between prefix and chord key must not cancel the
    // prefix: `;` is Shift+, on a Swedish layout (the isModifierKey guard).
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.down("Shift");
    await dpage.keyboard.up("Shift");
    await dpage.keyboard.press(";");
    await dpage.waitForTimeout(120);
    if ((await activePane()) !== "0") {
      fail(`⌃B ; broke when a bare Shift arrived first (active=${await activePane()})`);
      return;
    }
    if (!(await noCaretInField("⌃B ;"))) return;

    // Exactly one pane carries the focus ring, in every layout we touched.
    const ringCount = await dpage.locator("[data-pane-active]").count();
    if (ringCount !== 1) {
      fail(`expected exactly one [data-pane-active], saw ${ringCount}`);
      return;
    }
    // Part H's network half.
    if (labelPosts > 0) {
      fail(`pane switching POSTed /api/session/label ${labelPosts}× — an unasked rename (0081 Part H)`);
      return;
    }
    // Leave the grid single-pane so a human running this locally isn't handed a
    // 3-pane layout. (Hygiene only — every pass has its own storage partition.)
    await pickLayout("Single");
    if (derrs.length) fail("grid keyboard pass JS errors: " + derrs.join("; "));
  } catch (e) {
    if (derrs.length) console.error("grid keyboard JS errors:\n  " + derrs.join("\n  "));
    fail("grid keyboard pass: " + e.message);
  } finally {
    await dctx.close();
  }
}

// The other half of Part D: with GL unavailable the terminal must still mount,
// on xterm's DOM renderer, with search working there too.
async function domFallbackPass() {
  const nogl = await chromium.launch({
    executablePath: exe,
    headless: true,
    args: ["--no-sandbox", "--disable-3d-apis", "--disable-gpu"],
  });
  const dctx = await nogl.newContext({ viewport: { width: 1280, height: 820 } });
  const dpage = await dctx.newPage();
  const derrs = [];
  dpage.on("pageerror", (e) => derrs.push("pageerror: " + e.message));
  try {
    await mountDesktopSession(dpage);
    const renderer = await dpage.evaluate(() => window.__ccRenderer);
    if (renderer !== "dom") {
      fail(`expected the DOM fallback with GL disabled, got ${renderer ?? "undefined"}`);
      return;
    }
    // Find must work identically under the fallback.
    await dpage.locator(".xterm").first().click({ position: { x: 50, y: 60 } });
    await dpage.keyboard.press("Control+b");
    await dpage.keyboard.press("/");
    await dpage.waitForSelector('[data-testid="term-find"]', { timeout: 5000 });
    await dpage.keyboard.press("Escape");
    if (derrs.length) fail("DOM fallback pass JS errors: " + derrs.join("; "));
  } catch (e) {
    fail("DOM fallback pass: " + e.message);
  } finally {
    await dctx.close();
    await nogl.close();
  }
}

try {
  // Record everything the client puts on the terminal WebSocket. Proposals
  // 0031 C and 0077 A both turn on "what exactly reached the PTY" — the touch
  // ladder must send mouse/arrow bytes on the alternate screen and NOTHING on
  // the normal buffer, and the OSC 52 handler must never answer a query. An
  // init script is the only place this can be installed before the app
  // connects. Binary frames are decoded too: input can travel either way.
  await page.addInitScript(() => {
    window.__ccWire = [];
    const send = WebSocket.prototype.send;
    WebSocket.prototype.send = function (data) {
      try {
        if (typeof data === "string") {
          const m = JSON.parse(data);
          if (m && m.t === "i") window.__ccWire.push(m.d);
        } else {
          window.__ccWire.push(new TextDecoder().decode(data));
        }
      } catch {}
      return send.call(this, data);
    };
  });
  // The OSC 52 step reads the clipboard back; net-new harness setup (this suite
  // granted no clipboard permissions before).
  await ctx.grantPermissions(["clipboard-read", "clipboard-write"], { origin: base });
  await page.goto(base, { waitUntil: "networkidle" });

  // No last session => switcher auto-opens.
  await page.getByText("Sessions", { exact: true }).waitFor({ timeout: 8000 });

  // Pick our throwaway session (never the live ones).
  await page.getByText(session, { exact: false }).first().click({ timeout: 8000 });

  // WebSocket attach should reach "open" (header status dot's `data-conn`).
  await page.waitForSelector('[data-conn="open"]', { timeout: 10000 });

  // Control bar: arrows + Enter inject via /api/key.
  await page.getByRole("button", { name: "↑" }).click();
  await page.getByRole("button", { name: "↓" }).click();
  await page.getByRole("button", { name: "⏎ Enter" }).click();

  // Compose sheet: type and Send ⏎ -> /api/paste.
  await page.getByRole("button", { name: /Write a prompt/ }).click();
  await page.getByPlaceholder(/Write a prompt/).fill("echo SMOKE_COMPOSE_OK");
  await page.getByRole("button", { name: "Send ⏎" }).click();
  await page.waitForTimeout(600);

  // SIGINT preservation — the catastrophic correctness check for the
  // Cmd/Ctrl+C copy intercept in App.tsx. With NO selection, Ctrl+C must
  // still reach the terminal as 0x03 and produce a fresh prompt with ^C
  // echoed. If a future refactor accidentally preventDefaults the
  // no-selection branch, every Ctrl+C in the app becomes a dead key and
  // long-running processes can't be interrupted. We must catch that here.
  // Done before the swipe test below, which would leave tmux in copy-mode
  // and turn Ctrl+C into a "cancel copy-mode" instead of a SIGINT.
  await page.locator(".xterm").first().click({ position: { x: 50, y: 100 } });
  await page.waitForTimeout(100);
  await page.keyboard.press("Control+C");
  await page.waitForTimeout(400);
  const sigintPane = execFileSync("tmux", [
    "capture-pane", "-p", "-t", tmuxSession,
  ]).toString();
  const sigintOk = sigintPane.includes("^C");

  // Shift+drag must produce a real xterm.js selection (the visible
  // highlight depends on theme colours, but the API state is what Cmd/Ctrl+C
  // reads). We dispatch the events ourselves via page.evaluate with
  // explicit shiftKey/buttons fields, so Playwright's modifier-tracking
  // quirks can't mask the assertion. Target xterm's `.xterm-screen` (where
  // its mousedown listeners live) directly.
  const xt = await page.locator(".xterm").first().boundingBox();
  // Synthesize the full mousedown → mousemove → mouseup sequence with
  // shiftKey: true and detail: 1 (xterm.js's handleMouseDown branches on
  // click count; synthesized MouseEvents default to detail:0, which
  // silently no-ops the selection — that's the gotcha here, NOT a
  // Playwright limitation). Anchor at col 0 so the drag covers real
  // buffer content; mid-row clicks can land in trailing-whitespace and
  // make a valid selection still produce empty text.
  const xtermSelection = xt
    ? await page.evaluate(
        ({ x, y }) => {
          const term = (window).__ccTerm;
          if (!term) return "";
          const screen =
            document.querySelector(".xterm-screen") ||
            document.querySelector(".xterm");
          if (!screen) return "";
          const fire = (type, mx, my, buttons, detail) =>
            screen.dispatchEvent(
              new MouseEvent(type, {
                bubbles: true,
                cancelable: true,
                clientX: mx,
                clientY: my,
                button: 0,
                buttons,
                shiftKey: true,
                detail,
                view: window,
              })
            );
          fire("mousedown", x + 1, y + 30, 1, 1);
          for (let i = 1; i <= 10; i++) {
            fire("mousemove", x + 1 + i * 22, y + 30, 1, 0);
          }
          fire("mouseup", x + 250, y + 30, 0, 1);
          return term.getSelection?.() ?? "";
        },
        { x: xt.x, y: xt.y }
      )
    : "";
  const selectionOk = typeof xtermSelection === "string" && xtermSelection.length > 0;


  // ── Touch scroll: the [0069] ladder in the browser (proposal 0031 A+C) ─────
  //
  // The old assertion here checked that *tmux* had entered copy-mode. The
  // backend has been tmux-free for months, so it could not have been validating
  // the scrollLines() path it was meant to guard — the swipe was effectively
  // untested. It now asserts on the terminal itself, and on what the gesture
  // puts on the wire, which is what actually differs per rung.
  const cdp = await page.context().newCDPSession(page);
  const touch = (type, y, x = 195) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      touchPoints: type === "touchEnd" ? [] : [{ x, y }],
    });
  const swipe = async (from = 250, to = 600, step = 35) => {
    await touch("touchStart", from);
    for (let y = from; y <= to; y += step) {
      await touch("touchMove", y);
      await page.waitForTimeout(25);
    }
    await touch("touchEnd", to);
    await page.waitForTimeout(400);
  };
  // How far the viewport sits above live output. >0 means real scrollback moved.
  const viewportOffset = () =>
    page.evaluate(() => {
      const b = window.__ccTerm?.buffer?.active;
      return b ? b.baseY - b.viewportY : -1;
    });
  const wireSince = async (n = 0) =>
    page.evaluate((from) => (window.__ccWire || []).slice(from), n);
  // Put the child into a known mode by typing at its prompt.
  const shell = async (cmd) => {
    await page.locator(".xterm").first().click({ position: { x: 50, y: 100 } });
    await page.keyboard.type(cmd);
    await page.keyboard.press("Enter");
    await page.waitForTimeout(500);
  };

  // 1. touch_scroll_normal_buffer — the viewport moves, and the gesture puts
  //    NOTHING on the wire. A mis-gated ladder types arrow keys into a shell.
  const normalFrom = (await wireSince()).length;
  await swipe();
  const scrolled = (await viewportOffset()) > 0;
  const normalBufferSilent = (await wireSince(normalFrom)).length === 0;
  await page.evaluate(() => window.__ccTerm?.scrollToBottom());

  // 2. touch_scroll_alt_mouse — alternate screen + SGR mouse reporting is the
  //    Claude-fullscreen case: the drag speaks the app's own protocol and must
  //    NOT move the local viewport (the alt buffer has no scrollback to move).
  await shell("printf '\\033[?1049h\\033[?1006h\\033[?1002h'");
  const altMouseFrom = (await wireSince()).length;
  await swipe();
  const altMouseWire = (await wireSince(altMouseFrom)).join("");
  const altMouseOk = /\x1b\[<6[45];\d+;\d+M/.test(altMouseWire);
  const { cols: altCols, rows: altRows } = await page.evaluate(() => ({
    cols: window.__ccTerm?.cols ?? 0,
    rows: window.__ccTerm?.rows ?? 0,
  }));
  const coordsInRange = [...altMouseWire.matchAll(/\x1b\[<6[45];(\d+);(\d+)M/g)].every(
    (m) => +m[1] >= 1 && +m[1] <= altCols && +m[2] >= 1 && +m[2] <= altRows
  );
  const altViewportPinned = (await viewportOffset()) === 0;

  // 3. wheel_alt_screen_forwards (C4) — the desktop wheel over the same session
  //    already forwards as SGR reports; pinned so it can't break quietly.
  const wheelFrom = (await wireSince()).length;
  await page.locator(".xterm").first().hover();
  await page.mouse.wheel(0, -300);
  await page.waitForTimeout(300);
  const wheelOk = /\x1b\[<6[45];/.test((await wireSince(wheelFrom)).join(""));

  // 4. touch_scroll_fling_clamp (C3) — a fast fling stops emitting when the
  //    momentum decays: no backlog the application keeps chewing through long
  //    after the finger has left the glass.
  const flingFrom = (await wireSince()).length;
  await touch("touchStart", 700);
  for (let y = 700; y >= 120; y -= 90) {
    await touch("touchMove", y);
    await page.waitForTimeout(8);
  }
  await touch("touchEnd", 120);
  await page.waitForTimeout(1200);
  const afterFling = (await wireSince(flingFrom)).length;
  await page.waitForTimeout(1200);
  const flingSettled = (await wireSince(flingFrom)).length === afterFling;

  // 5. touch_scroll_alt_arrows — the alternate screen WITHOUT mouse reporting
  //    (less, a plain vim) pages the application with arrow keys instead.
  await shell("printf '\\033[?1002l\\033[?1006l'");
  const arrowsFrom = (await wireSince()).length;
  await swipe();
  const arrowsWire = (await wireSince(arrowsFrom)).join("");
  const altArrowsOk = /\x1b(\[|O)[AB]/.test(arrowsWire) && !/\x1b\[</.test(arrowsWire);

  // 6. touch_scroll_alt_exit — leaving the alternate screen returns the swipe to
  //    local scrollback within one gesture, no reload (the onBufferChange path).
  await shell("printf '\\033[?1049l'");
  await swipe();
  const altExitOk = (await viewportOffset()) > 0;
  await page.evaluate(() => window.__ccTerm?.scrollToBottom());

  // ── OSC 52: a copy performed INSIDE the session (proposal 0077 A/E) ────────
  //
  // The headline flow: the session emits the sequence Claude Code emits on
  // every copy, and the BROWSER's clipboard ends up holding the text. The
  // driver gate is satisfied because this client has been typing into this
  // focused pane throughout — which is the point of the gate.
  await shell("printf '\\033]52;c;U01PS0VfQ0xJUA==\\007'"); // "SMOKE_CLIP"
  await page.waitForTimeout(800);
  const clipText = await page.evaluate(() =>
    navigator.clipboard.readText().catch(() => "")
  );
  const osc52WriteOk = clipText === "SMOKE_CLIP";

  // The query form must NEVER be answered. The assertion is on the wire: a real
  // answer would be ESC]52;c;<base64>BEL travelling UP it.
  const queryFrom = (await wireSince()).length;
  await shell("printf '\\033]52;c;?\\007'");
  await page.waitForTimeout(500);
  const osc52QuerySilent = !/\x1b\]52;/.test((await wireSince(queryFrom)).join(""));

  // Image paste: open the sheet, choose a real PNG, send -> POST /api/clip.
  // Scope to the image sheet's input (accept="image/*") — the footer's Upload
  // picker is also an input[type=file], so the bare selector is ambiguous now.
  await page.getByRole("button", { name: "Paste an image" }).click();
  await page.locator('input[accept="image/*"]').setInputFiles(
    new URL("../public/favicon.png", import.meta.url).pathname
  );
  await page.getByRole("button", { name: /Paste into terminal/ }).click({ timeout: 5000 });
  await page.waitForTimeout(400);

  // File upload (phone): the footer Upload button opens the OS picker (a
  // multiple, accept-less input[type=file]) and routes the choice into the
  // UploadSheet, which preselects the project root. Upload there -> the file
  // lands in the session cwd. Distinct buffer/name so it's unambiguous to
  // assert + clean up.
  await page.getByRole("button", { name: "Upload files or photos" }).click();
  await page.locator('input[type="file"]:not([accept])').setInputFiles({
    name: uploadName,
    mimeType: "text/plain",
    buffer: Buffer.from("smoke upload\n"),
  });
  // Default destination is the project root; the button enables once it loads.
  await page.getByRole("button", { name: /Upload 1 file here/ }).click({ timeout: 8000 });
  await page.waitForTimeout(800);
  const uploadOk = !!uploadedPath && existsSync(uploadedPath);

  // --- File editor (phone) ---
  // Open the Files sheet, tap the markdown file -> the editor overlay opens with
  // live-preview rendering. Exercise both save paths (autosave on by default,
  // then manual ⌘/Ctrl+S), toggle the reading view, create a new file, close.
  await page.getByRole("button", { name: "Browse, view and download files" }).click();
  // The editor tree is project-first; the throwaway file is in the share section,
  // collapsed by default, so expand it before picking. Each file row is split
  // into an open button (name = the bare filename) and a "Download <name>"
  // button, so match the open one by exact name. The expand is idempotent —
  // clicking an already-open section would collapse it — so only expand when the
  // row isn't already showing (the share section's expanded state persists).
  if ((await page.getByRole("button", { name: "ccwebsmoke_edit.md", exact: true }).count()) === 0) {
    await page.getByText("Share folder", { exact: true }).click();
  }
  await page.getByRole("button", { name: "ccwebsmoke_edit.md", exact: true }).click({ timeout: 8000 });
  // Live preview styled the heading line (cm-md-h1 is applied regardless of
  // cursor position; only the `#` mark hides off the active line).
  await page.waitForSelector(".cm-md-h1", { timeout: 8000 });
  // 1) Auto-save (default ON): type, wait for the debounce to flush to disk —
  //    no Save click needed.
  await page.locator(".cm-content").click();
  await page.keyboard.type("EDITED ");
  await page.waitForTimeout(1300);
  const editorSaveOk = readFileSync(editFile, "utf8").includes("EDITED");
  // 2) Manual save: flip auto-save OFF, edit, then press Ctrl+S and assert it
  //    reached disk (the keyboard path must work in manual mode). On a phone
  //    the toolbar folds font/auto-save/new/delete into a "⋯" overflow menu,
  //    so those three steps go through it. The Auto-save item leaves the menu
  //    open (it's a toggle), so Escape dismisses it before clicking the editor.
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitemcheckbox", { name: /Auto-save/ }).click();
  await page.keyboard.press("Escape");
  await page.locator(".cm-content").click();
  await page.keyboard.type("MANUAL ");
  await page.keyboard.press("Control+s");
  await page.waitForTimeout(600);
  const editorManualOk = readFileSync(editFile, "utf8").includes("MANUAL");
  // Reading view renders markdown (react-markdown -> .cc-prose). The toggle's
  // icon flips Read<->Edit, so target it by its stable title (kept in the
  // phone toolbar, not the menu).
  await page.locator('button[title="Toggle reading view"]').click();
  await page.waitForSelector(".cc-prose h1", { timeout: 5000 });
  await page.locator('button[title="Toggle reading view"]').click();
  // New file (⋯ menu item): created in the current file's folder (share dir).
  // Selecting it closes the menu and opens the name field.
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "New file" }).click();
  await page.getByPlaceholder("notes.md").fill(newMdName);
  await page.getByRole("button", { name: "Create", exact: true }).click();
  await page.waitForTimeout(500);
  const editorNewOk = existsSync(newMdPath);
  // Delete (⋯ menu item): the new file is now open — trash it via the menu +
  // confirm bar, and assert it's gone from disk.
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Delete file" }).click();
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.waitForTimeout(500);
  const editorDeleteOk = !existsSync(newMdPath);
  // --- PDF viewing (phone) ---
  // The delete above left us on the full-screen file tree. Expand the share
  // section and open the throwaway PDF -> the editor routes it to the pdf.js
  // viewer (read-only). Assert a page canvas actually rasterises (backing px)
  // and that the inline byte stream (?inline=1) was fetched, then step back to
  // the tree so the "Close editor" below still applies.
  if ((await page.getByRole("button", { name: "ccwebsmoke_doc.pdf", exact: true }).count()) === 0) {
    await page.getByText("Share folder", { exact: true }).click();
  }
  await page.getByRole("button", { name: "ccwebsmoke_doc.pdf", exact: true }).click({ timeout: 8000 });
  let pdfRendered = false;
  try {
    await page.waitForFunction(
      () => {
        const c = document.querySelector("canvas.cc-pdf-canvas");
        return !!c && c.width > 0 && c.height > 0;
      },
      { timeout: 12000 }
    );
    pdfRendered = true;
  } catch {}
  await page.getByRole("button", { name: "Back to files" }).click();
  await page.waitForTimeout(150);

  // After delete the phone lands on the full-screen file tree (z-70, over the
  // toolbar); its ✕ exits the overlay. With no file open both the panel ✕ and
  // the now-covered toolbar ✕ read "Close editor", so take the last (panel).
  await page.getByRole("button", { name: "Close editor" }).last().click();
  await page.waitForTimeout(200);

  // Session delete: open the drawer and hard-kill the throwaway 'ccwebdel'.
  await page.getByRole("button", { name: "Open sessions" }).click();
  // Team-tier gating (0065 Part F): the team chip and the ~/team dashboard
  // window are multi-tenant-org surfaces and must NEVER render against a
  // single-tenant hub (which is what this smoke targets). Checked here with
  // the drawer open — the chip's only home — before we mutate the list.
  const teamChipLeak = await page.getByText("team", { exact: true }).count();
  const teamWindowLeak = await page.getByText("~/team", { exact: false }).count();
  await page.getByRole("button", { name: "Delete session ccwebdel" }).click();
  await page.getByRole("button", { name: "kill", exact: true }).click();
  await page
    .locator('[aria-label="Delete session ccwebdel"]')
    .waitFor({ state: "detached", timeout: 10000 });

  // New-session browser: (drawer already open) start a new session, browse one
  // level deep. We do NOT create (that would launch a real agent).
  await page.getByRole("button", { name: /New session/ }).click();
  await page.getByText("New session", { exact: true }).waitFor({ timeout: 5000 });
  await page.waitForTimeout(300);

  // Create a folder (at home), confirm it appears, then delete it.
  const fname = "ccwebsmoke_" + Date.now();
  await page.getByRole("button", { name: "＋📁" }).click();
  await page.getByPlaceholder("new folder name").fill(fname);
  // Two "Add" buttons exist for an extra-dirs-capable tool: the folder-create
  // confirm (first, bg-accent) and the Extra-folders picker opener. Take first.
  await page.getByRole("button", { name: "Add", exact: true }).first().click();
  await page.locator(`[data-folder="${fname}"]`).waitFor({ timeout: 5000 });
  const created = true;
  await page.getByRole("button", { name: `Delete folder ${fname}` }).click();
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.waitForTimeout(600);
  const deleted = (await page.locator(`[data-folder="${fname}"]`).count()) === 0;

  // Proposal 0082: the assistant-remote-control switch is capability-gated and
  // defaults OFF. Claude has a remote control to turn on; the bare shell tool
  // doesn't, and a switch that changes nothing must never be offered.
  const toolChip = (prefix) => page.locator("button", { hasText: new RegExp(`^${prefix}$`, "i") });
  await toolChip("claude").first().click();
  await page.waitForTimeout(150);
  const rcChip = page.locator("[data-rc-switch]");
  const rcOffered = (await rcChip.count()) === 1;
  const rcDefaultsOff = rcOffered && (await rcChip.getAttribute("data-rc-on")) === "0";
  await toolChip("shell").first().click();
  await page.waitForTimeout(150);
  const rcHiddenForShell = (await page.locator("[data-rc-switch]").count()) === 0;
  await toolChip("claude").first().click();
  await page.waitForTimeout(150);

  // Descend into a folder (dir navigation).
  const folder = page.locator("[data-folder]").first();
  if (await folder.count()) await folder.click();
  await page.waitForTimeout(300);

  const keyCalls = api.filter((a) => a.startsWith("POST /api/key")).length;
  const pasteCalls = api.filter((a) => a.startsWith("POST /api/paste")).length;
  const wsOpened = api.some((a) => a.startsWith("WS"));
  const strayWs = api.filter((a) => a.startsWith("WS") && !a.includes(session));
  const clipCalls = api.filter((a) => a.startsWith("POST /api/clip")).length;
  const toolCalls = api.filter((a) => a.startsWith("GET /api/tools")).length;
  const dirCalls = api.filter((a) => a.startsWith("GET /api/dirs")).length;
  const mkdirOk = api.some((a) => a.startsWith("POST /api/mkdir"));
  const rmdirOk = api.some((a) => a.startsWith("POST /api/rmdir"));
  const delOk = api.some((a) => a.startsWith("POST /api/session/delete"));
  const readOk = api.some((a) => a.startsWith("GET /api/file/read"));
  const writeOk = api.filter((a) => a.startsWith("POST /api/file/write")).length;
  const pdfInlineOk = api.some((a) => a.startsWith("GET /api/download") && a.includes("inline=1"));

  if (errors.length) fail("JS errors present");
  else if (teamChipLeak > 0 || teamWindowLeak > 0)
    fail(`team UI leaked into single-tenant (chip=${teamChipLeak}, ~/team window=${teamWindowLeak})`);
  else if (keyCalls < 3) fail(`expected >=3 /api/key calls, got ${keyCalls}`);
  else if (pasteCalls < 1) fail(`expected a /api/paste call, got ${pasteCalls}`);
  else if (!wsOpened) fail("terminal WebSocket never opened");
  else if (strayWs.length) fail(`attached a session before the user picked: ${strayWs.join(", ")}`);
  else if (!sigintOk) fail("Ctrl+C with no selection did not reach the shell as SIGINT (copy intercept regressed)");
  else if (!selectionOk) fail("Shift+drag did not produce an xterm.js selection (term.getSelection() empty)");
  else if (!scrolled) fail("swipe did not scroll the terminal viewport (0031 rung 1)");
  else if (!normalBufferSilent) fail("swipe on the NORMAL buffer put input on the wire (0031 C rung 1 must send nothing)");
  else if (!altMouseOk) fail("swipe on a mouse-reporting alt screen sent no SGR wheel reports (0031 C rung 2)");
  else if (!coordsInRange) fail("SGR wheel reports carried coordinates outside 1..cols / 1..rows (0031 C2)");
  else if (!altViewportPinned) fail("swipe on the alt screen moved the local viewport (it has no scrollback)");
  else if (!wheelOk) fail("desktop wheel stopped forwarding to a mouse-reporting alt screen (0031 C4)");
  else if (!flingSettled) fail("a fling kept emitting input after the momentum decayed (0031 C3 clamp)");
  else if (!altArrowsOk) fail("alt screen without mouse reporting did not page with arrow keys (0031 C rung 3)");
  else if (!altExitOk) fail("leaving the alt screen did not return the swipe to local scrollback (onBufferChange)");
  else if (!osc52WriteOk) fail("OSC 52 from the session did not reach the browser clipboard (0077 A)");
  else if (!osc52QuerySilent) fail("the client answered an OSC 52 query — the read form must NEVER be answered (0077 A1)");
  else if (clipCalls < 1) fail(`expected a /api/clip image upload, got ${clipCalls}`);
  else if (!api.some((a) => a.startsWith("POST /api/upload?"))) fail("phone Upload button never POSTed /api/upload");
  else if (!uploadOk) fail("phone upload didn't land in the session cwd");
  else if (toolCalls < 1) fail("new-session panel didn't load tools");
  else if (!rcOffered) fail("claude's assistant-remote-control switch is missing from the create sheet (0082 B)");
  else if (!rcDefaultsOff) fail("the assistant-remote-control switch didn't default to off (0082 B)");
  else if (!rcHiddenForShell) fail("the assistant-remote-control switch was offered for the shell tool (0082 B)");
  else if (dirCalls < 2) fail(`expected dir browse + descend, got ${dirCalls} /api/dirs`);
  else if (!created || !mkdirOk) fail("folder create didn't work");
  else if (!deleted || !rmdirOk) fail("folder delete didn't work");
  else if (!delOk) fail("session delete didn't fire");
  else if (!readOk) fail("editor never read a file (GET /api/file/read)");
  else if (writeOk < 2) fail(`expected >=2 /api/file/write (autosave + manual + new), got ${writeOk}`);
  else if (!editorSaveOk) fail("editor autosave didn't reach disk");
  else if (!editorManualOk) fail("editor manual Ctrl+S didn't reach disk");
  else if (!editorNewOk) fail("editor new-file didn't create the file");
  else if (!editorDeleteOk) fail("editor delete didn't remove the file");
  else if (!api.some((a) => a.startsWith("POST /api/file/delete"))) fail("editor delete never called /api/file/delete");
  else if (!pdfRendered) fail("PDF viewer didn't rasterise a page canvas");
  else if (!pdfInlineOk) fail("PDF viewer didn't fetch the inline byte stream (?inline=1)");
  else {
    await desktopEditorPass();
    if (process.exitCode === 1) throw new Error("desktop editor pass failed");
    await idleRendererPass();
    if (process.exitCode === 1) throw new Error("idle/renderer pass failed");
    await domFallbackPass();
    if (process.exitCode === 1) throw new Error("DOM fallback pass failed");
    await recentSectionPass();
    if (process.exitCode === 1) throw new Error("recent-section pass failed");
    await gridKeyboardPass();
    if (process.exitCode === 1) throw new Error("grid keyboard pass failed");
    await deepLinkPass();
    if (process.exitCode === 1) throw new Error("deep-link pass failed");
    console.log("SMOKE PASS (touch ladder: scrollback + wheel reports + arrows; OSC 52 clipboard; editor save/new/read OK; webgl+dom renderers, idle quiescent, ⌃B / find OK; Recent section OK; grid keymap: wrap + empty-pane prefix + ⌃B ; + no unasked rename OK; file deep links: desktop + phone zero-tap, folder form, heal, Copy link OK)");
    console.log("API calls:\n  " + api.join("\n  "));
  }
} catch (e) {
  fail(e.message);
} finally {
  rmSync(editFile, { force: true });
  rmSync(newMdPath, { force: true });
  rmSync(pdfPath, { force: true });
  if (uploadedPath) rmSync(uploadedPath, { force: true });
  await browser.close();
}
