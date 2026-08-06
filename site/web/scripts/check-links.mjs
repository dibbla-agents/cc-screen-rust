// check-links.mjs — internal-link checker for the emitted docs tree (0055).
//
//   node scripts/check-links.mjs ../../docs
//
// Walks every .html under the given root, extracts href/src/poster/data-cast
// attributes, and fails (exit 1) on any *internal* reference that doesn't
// resolve to a file in the tree. External links (http/https/mailto), pure
// fragments, and protocol-relative URLs are skipped — this guards the docs'
// own cross-links, which the generator emits relatively (../security/), not
// the wider web. A second pass (0061 D4) cross-checks the docs *sources*
// against the capture manifest — see below.

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = process.argv[2];
if (!root) {
  console.error("usage: node scripts/check-links.mjs <emitted-tree-root>");
  process.exit(2);
}
const rootAbs = path.resolve(root);
if (!existsSync(rootAbs)) {
  console.error(`check-links: no such directory: ${rootAbs}`);
  process.exit(2);
}

function* htmlFiles(dir) {
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    if (statSync(p).isDirectory()) yield* htmlFiles(p);
    else if (name.endsWith(".html")) yield p;
  }
}

const SKIP = /^(https?:|mailto:|tel:|data:|#|\/\/)/i;
const ATTR = /(?:href|src|poster|data-cast)\s*=\s*"([^"]+)"/g;

let checked = 0;
const dead = [];

for (const file of htmlFiles(rootAbs)) {
  const html = readFileSync(file, "utf8");
  for (const [, raw] of html.matchAll(ATTR)) {
    if (SKIP.test(raw)) continue;
    const target = raw.split("#")[0].split("?")[0];
    if (!target) continue; // fragment-only
    checked++;
    // Root-absolute targets resolve against the tree root; relative against the file.
    const base = target.startsWith("/")
      ? path.join(rootAbs, target)
      : path.resolve(path.dirname(file), target);
    const ok =
      (existsSync(base) && statSync(base).isFile()) ||
      existsSync(path.join(base, "index.html")); // pretty URL → dir with index.html
    if (!ok) dead.push(`${path.relative(rootAbs, file)} → ${raw}`);
  }
}

// ── asset ⇄ manifest cross-check (0061 D4) ────────────────────────────────
// The docs sources (site/docs/*.md) and the capture manifest must agree, both
// directions: every img/media asset a page embeds exists on disk and is some
// manifest entry's output; every manifest docs-asset is embedded by some page.
// Entries whose captures haven't landed yet (file absent, or blockedOn) are
// notices, not failures — captures ship in their own wave; only a *committed*
// orphan or a page pointing at nothing breaks the build.
const SRCDOCS = fileURLToPath(new URL("../../docs/", import.meta.url));
const REPO = fileURLToPath(new URL("../../../", import.meta.url));
const manifest = (await import("../../capture/manifest.mjs")).default;
const outPaths = new Set(manifest.flatMap((e) => [...(e.out ?? []), ...(e.poster ? [e.poster] : [])]));
const MDREF = /!\[[^\]]*\]\(([^)\s]+)/g; // markdown images
const referenced = new Set(); // repo-relative asset paths some page embeds

for (const file of readdirSync(SRCDOCS).filter((f) => f.endsWith(".md") && f !== "README.md")) {
  const md = readFileSync(path.join(SRCDOCS, file), "utf8");
  const slug = file.replace(/\.md$/, "");
  // pages emit to docs/docs/<slug>/, so their relative refs resolve one level
  // below the source dir (index.md emits to docs/docs/ itself).
  const baseDir = slug === "index" ? SRCDOCS : path.join(SRCDOCS, slug);
  for (const re of [MDREF, ATTR]) {
    for (const [, raw] of md.matchAll(re)) {
      if (SKIP.test(raw) || raw.endsWith("/")) continue; // pages, not assets
      const abs = path.resolve(baseDir, raw.split("#")[0].split("?")[0]);
      const rel = path.relative(REPO, abs).split(path.sep).join("/");
      referenced.add(rel);
      checked++;
      if (!existsSync(abs)) dead.push(`${file} → ${raw} (missing on disk)`);
      else if (/^site\/docs\/(img|media)\//.test(rel) && !outPaths.has(rel))
        dead.push(`${file} → ${raw} (not in site/capture/manifest.mjs)`);
    }
  }
}

for (const entry of manifest) {
  const docsOuts = [...(entry.out ?? []), ...(entry.poster ? [entry.poster] : [])]
    .filter((p) => p.startsWith("site/docs/"));
  if (docsOuts.length === 0 || docsOuts.some((p) => referenced.has(p))) continue;
  if (entry.blockedOn)
    console.log(`check-links: notice: manifest '${entry.id}' skipped (blockedOn: ${entry.blockedOn})`);
  else if (docsOuts.some((p) => existsSync(path.join(REPO, p))))
    dead.push(`manifest '${entry.id}' → ${docsOuts.join(", ")} (committed but referenced by no page)`);
  else console.log(`check-links: notice: manifest '${entry.id}' pending capture (${docsOuts.join(", ")})`);
}

if (dead.length) {
  console.error(`check-links: ${dead.length} dead internal link(s):`);
  for (const d of dead) console.error(`  ${d}`);
  process.exit(1);
}
console.log(`check-links: ${checked} internal references OK under ${path.relative(process.cwd(), rootAbs) || "."}`);
