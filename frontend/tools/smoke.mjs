// Headless end-to-end smoke test of the built PWA in a phone viewport.
// Loads the page, switches to a throwaway session, exercises the arrow/Enter
// control keys and the compose sheet, and fails on any console/page error.
import { chromium } from "playwright";
import { execFileSync } from "node:child_process";
import { writeFileSync, readFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const base = process.env.BASE || "http://127.0.0.1:8840";
const exe = process.env.CHROME;
// Short name of the throwaway session to drive (never a live one).
const session = process.env.SESSION || "smoketest";
// Full tmux name, for asserting the swipe actually scrolled (copy-mode).
const tmuxSession = process.env.TMUX_SESSION || `claude-${session}`;

const browser = await chromium.launch({
  executablePath: exe,
  headless: true,
  args: ["--no-sandbox", "--disable-gpu"],
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
    // Desktop shows an inline picker in the empty pane — mount the throwaway
    // session so the pane gets its top-right chrome (the editor button).
    await dpage.getByText("Empty pane", { exact: false }).waitFor({ timeout: 8000 });
    await dpage.getByRole("button", { name: new RegExp(session) }).first().click({ timeout: 8000 });
    await dpage.waitForSelector('[title="open"]', { timeout: 10000 });
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

try {
  await page.goto(base, { waitUntil: "networkidle" });

  // No last session => switcher auto-opens.
  await page.getByText("Sessions", { exact: true }).waitFor({ timeout: 8000 });

  // Pick our throwaway session (never the live ones).
  await page.getByText(session, { exact: false }).first().click({ timeout: 8000 });

  // WebSocket attach should reach "open" (header status dot title).
  await page.waitForSelector('[title="open"]', { timeout: 10000 });

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


  // Swipe-to-scroll: a finger drag DOWN over the terminal should scroll back
  // (tmux enters copy-mode). Simulated via CDP touch events.
  const cdp = await page.context().newCDPSession(page);
  const touch = (type, y) =>
    cdp.send("Input.dispatchTouchEvent", {
      type,
      touchPoints: type === "touchEnd" ? [] : [{ x: 195, y }],
    });
  await touch("touchStart", 250);
  for (let y = 250; y <= 600; y += 35) {
    await touch("touchMove", y);
    await page.waitForTimeout(25);
  }
  await touch("touchEnd", 600);
  await page.waitForTimeout(400);
  const inMode = execFileSync("tmux", [
    "display-message", "-p", "-t", tmuxSession, "#{pane_in_mode}",
  ]).toString().trim();
  const scrolled = inMode === "1";

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
  else if (keyCalls < 3) fail(`expected >=3 /api/key calls, got ${keyCalls}`);
  else if (pasteCalls < 1) fail(`expected a /api/paste call, got ${pasteCalls}`);
  else if (!wsOpened) fail("terminal WebSocket never opened");
  else if (strayWs.length) fail(`attached a session before the user picked: ${strayWs.join(", ")}`);
  else if (!sigintOk) fail("Ctrl+C with no selection did not reach the shell as SIGINT (copy intercept regressed)");
  else if (!selectionOk) fail("Shift+drag did not produce an xterm.js selection (term.getSelection() empty)");
  else if (!scrolled) fail("swipe did not scroll the terminal (tmux not in copy-mode)");
  else if (clipCalls < 1) fail(`expected a /api/clip image upload, got ${clipCalls}`);
  else if (!api.some((a) => a.startsWith("POST /api/upload?"))) fail("phone Upload button never POSTed /api/upload");
  else if (!uploadOk) fail("phone upload didn't land in the session cwd");
  else if (toolCalls < 1) fail("new-session panel didn't load tools");
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
    console.log("SMOKE PASS (swipe scrolled into copy-mode; editor save/new/read OK)");
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
