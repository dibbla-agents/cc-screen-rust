// build-docs.mjs — the cc-screen docs generator (proposal 0055).
//
// Renders the markdown sources in site/docs/*.md into docs/docs/<slug>/index.html
// with a hand-written shell that shares the marketing site's palette. Runs AFTER
// `vite build` (which wipes docs/ via emptyOutDir) — it is the last step of
// `npm run build`, and `npm run docs` runs it alone to iterate on docs.
//
// Deliberately boring: one dependency (marked), a ~15-line front-matter parser,
// a template literal for the shell. If this file needs to grow past ~150 lines
// of logic (search, versioned trees…), that's the trigger to revisit VitePress.

import { marked } from "marked";
import { cpSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const SRC = new URL("../../docs/", import.meta.url); //   site/docs/   (source)
const OUT = new URL("../../../docs/docs/", import.meta.url); // docs/docs/ (emit)

const nav = JSON.parse(readFileSync(new URL("nav.json", SRC), "utf8"));

// Version stamp for the footer — single source of truth is the workspace version.
const cargo = readFileSync(new URL("../../../Cargo.toml", import.meta.url), "utf8");
const version = /^\s*version\s*=\s*"(\d+\.\d+\.\d+)"/m.exec(cargo)?.[1];
if (!version) throw new Error("build-docs: could not read the workspace version from Cargo.toml");

// ── nav ↔ sources must agree, both directions (dead pages can't ship silently) ─
// README.md is contributor notes about this directory, not a page.
const mdFiles = readdirSync(fileURLToPath(SRC)).filter((f) => f.endsWith(".md") && f !== "README.md");
const slugs = mdFiles.map((f) => f.replace(/\.md$/, ""));
const navSlugs = nav.sections.flatMap((s) => s.pages);
const missing = navSlugs.filter((s) => !slugs.includes(s));
const unlisted = slugs.filter((s) => !navSlugs.includes(s));
if (missing.length || unlisted.length) {
  if (missing.length) console.error(`build-docs: nav.json lists pages with no .md source: ${missing.join(", ")}`);
  if (unlisted.length) console.error(`build-docs: .md files not listed in nav.json: ${unlisted.join(", ")}`);
  process.exit(1);
}

// ── front-matter: a flat `key: value` block between --- fences ────────────────
function frontMatter(raw) {
  const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(raw);
  if (!m) return { attrs: {}, body: raw };
  const attrs = {};
  for (const line of m[1].split(/\r?\n/)) {
    const kv = /^([A-Za-z_-]+):\s*(.*)$/.exec(line.trim());
    if (kv) attrs[kv[1]] = kv[2].trim();
  }
  return { attrs, body: raw.slice(m[0].length) };
}

const esc = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

// Heading ids so in-page anchors work (marked no longer adds them itself).
const slugify = (t) =>
  t.toLowerCase().replace(/<[^>]+>/g, "").replace(/&[a-z]+;/g, "").replace(/[^a-z0-9\s-]/g, "").trim().replace(/\s+/g, "-");
marked.use({
  renderer: {
    heading({ tokens, depth }) {
      const text = this.parser.parseInline(tokens);
      return `<h${depth} id="${slugify(text)}">${text}</h${depth}>\n`;
    },
  },
});

// ── the sidebar (rendered twice: <details> on mobile, plain <nav> on desktop) ─
function navList(current, docsRoot) {
  const items = nav.sections
    .map((section) => {
      const links = section.pages
        .map((slug) => {
          const href = docsRoot + (slug === "index" ? "" : `${slug}/`);
          const title = pageTitles.get(slug) ?? slug;
          const cur = slug === current ? ' aria-current="page"' : "";
          return `        <li><a href="${href}"${cur}>${esc(title)}</a></li>`;
        })
        .join("\n");
      return `      <div class="nav-section">\n        <div class="nav-label">${esc(section.label)}</div>\n        <ul>\n${links}\n        </ul>\n      </div>`;
    })
    .join("\n");
  return items;
}

// ── the shell ─────────────────────────────────────────────────────────────────
function shell({ slug, title, description, content }) {
  const docsRoot = slug === "index" ? "" : "../"; //   → /docs/
  const siteRoot = slug === "index" ? "../" : "../../"; // → /
  const sidebar = navList(slug, docsRoot);
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>${esc(title)} — cc-screen docs</title>
<meta name="description" content="${esc(description)}" />
<meta name="theme-color" content="#060e09" />
<link rel="icon" href="${siteRoot}favicon.svg" type="image/svg+xml" />
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
<link href="https://fonts.googleapis.com/css2?family=Hanken+Grotesk:wght@400;500;600&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet" />
<link rel="stylesheet" href="${docsRoot}docs.css" />
</head>
<body>
<header class="docs-header">
  <a class="wordmark" href="${siteRoot}"><span class="prompt">&gt;_</span>cc-screen</a>
  <span class="docs-tag">docs</span>
  <a class="signin" href="https://app.ccscreen.dev">Sign in ↗</a>
</header>
<div class="docs-layout">
  <details class="docs-nav mobile-nav">
    <summary>On this site</summary>
    <nav>
${sidebar}
    </nav>
  </details>
  <aside class="docs-nav desktop-nav">
    <nav>
${sidebar}
    </nav>
  </aside>
  <main class="docs-content">
${content}
  </main>
</div>
<footer class="docs-footer">
  <span>v${version} — docs track the latest release</span>
  <a href="https://github.com/dibbla-agents/cc-screen-rust">GitHub</a>
</footer>
</body>
</html>
`;
}

// ── emit ──────────────────────────────────────────────────────────────────────
// First pass: front-matter for every page (nav titles), then render each.
const pages = new Map();
const pageTitles = new Map();
for (const file of mdFiles) {
  const { attrs, body } = frontMatter(readFileSync(new URL(file, SRC), "utf8"));
  const slug = file.replace(/\.md$/, "");
  if (!attrs.title) throw new Error(`build-docs: ${file} has no front-matter title`);
  pages.set(slug, { attrs, body });
  pageTitles.set(slug, attrs.nav || attrs.title);
}

for (const [slug, { attrs, body }] of pages) {
  const html = shell({
    slug,
    title: attrs.title,
    description: attrs.description ?? "",
    content: marked.parse(body),
  });
  const dir = new URL(slug === "index" ? "./" : `${slug}/`, OUT);
  mkdirSync(dir, { recursive: true });
  writeFileSync(new URL("index.html", dir), html);
}

cpSync(new URL("img/", SRC), new URL("img/", OUT), { recursive: true });
cpSync(new URL("docs.css", SRC), new URL("docs.css", OUT));
console.log(`build-docs: emitted ${pages.size} pages → docs/docs/ (v${version})`);
