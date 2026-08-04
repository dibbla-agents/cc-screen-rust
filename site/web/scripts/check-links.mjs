// check-links.mjs — internal-link checker for the emitted docs tree (0055).
//
//   node scripts/check-links.mjs ../../docs
//
// Walks every .html under the given root, extracts href/src attributes, and
// fails (exit 1) on any *internal* reference that doesn't resolve to a file in
// the tree. External links (http/https/mailto), pure fragments, and protocol-
// relative URLs are skipped — this guards the docs' own cross-links, which the
// generator emits relatively (../security/), not the wider web.

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";

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
const ATTR = /(?:href|src)\s*=\s*"([^"]+)"/g;

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

if (dead.length) {
  console.error(`check-links: ${dead.length} dead internal link(s):`);
  for (const d of dead) console.error(`  ${d}`);
  process.exit(1);
}
console.log(`check-links: ${checked} internal references OK under ${path.relative(process.cwd(), rootAbs) || "."}`);
