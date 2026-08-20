// Turning a clipboard payload into a GFM table (proposal 0085 Part D).
//
// Two pure converters, no DOM mutation, no CodeMirror: the paste handler in
// livePreview.ts owns the wiring, these own the parsing. Both are deliberately
// conservative — they return null the moment the payload isn't obviously a
// table, because a false negative pastes plain text (what the editor always
// did) while a false positive mangles the user's paste.
//
// SECURITY: `htmlTableToMarkdown` reads `textContent` and nothing else. Markup
// in a hostile `text/html` flavour is never interpreted, never re-emitted, and
// never reaches the document: what comes out is plain text that
// `serializeTable` pipe-escapes. Do not "improve" this by preserving inline
// HTML, and do not hand the parsed document to anything that renders it.

import type { TableAlign, TableData } from "./livePreview";

// One cell's text, flattened: any run of whitespace (including the internal
// newlines a spreadsheet cell can hold) collapses to a single space, so a cell
// can never break out of its row. `|` is left alone here — serializeTable
// escapes it, in the one place the escaping rules live.
function flattenCell(s: string): string {
  return s.replace(/\u00a0/g, " ").replace(/\s+/g, " ").trim();
}

function toTable(rows: string[][]): TableData | null {
  if (rows.length < 2) return null;
  let cols = 0;
  for (const r of rows) cols = Math.max(cols, r.length);
  if (cols < 1) return null;
  const align: TableAlign[] = new Array(cols).fill(null);
  return { header: rows[0], align, body: rows.slice(1) };
}

// tsvToTable converts classic tab-separated text. The bar is: at least two
// non-blank lines, and EVERY non-blank line carries at least one tab. Code with
// tabs on only some lines, prose, and single-cell copies all fail it and fall
// through to the default paste. Ragged rows are kept — serializeTable pads them.
export function tsvToTable(text: string): TableData | null {
  const lines = text
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .filter((l) => l.trim().length > 0);
  if (lines.length < 2) return null;
  if (lines.some((l) => !l.includes("\t"))) return null;
  return toTable(lines.map((l) => l.split("\t").map(flattenCell)));
}

// htmlTableToMarkdown converts the `text/html` flavour Excel, Google Sheets and
// Numbers put on the clipboard. It requires exactly one <table> and no
// meaningful text outside it (the wrappers those apps emit are <meta>/<style>
// noise, which carries none). Alignment is not inferred in v1 — inline
// `text-align` is ignored and every column comes out unaligned; colspan/rowspan
// are ignored too, so a merged-cell table converts to its flat cell text.
export function htmlTableToMarkdown(html: string): TableData | null {
  if (typeof DOMParser === "undefined") return null;
  let doc: Document;
  try {
    doc = new DOMParser().parseFromString(html, "text/html");
  } catch {
    return null;
  }
  const body = doc.body;
  if (!body) return null;
  const tables = body.querySelectorAll("table");
  if (tables.length !== 1) return null;
  const table = tables[0];

  // Nothing but the table: clone the body, drop the table and the non-content
  // elements the clipboard wrappers carry, and require what's left to be blank.
  const rest = body.cloneNode(true) as HTMLElement;
  rest.querySelectorAll("table, style, script, meta, link, title").forEach((n) => n.remove());
  if (flattenCell(rest.textContent ?? "") !== "") return null;

  const rows: string[][] = [];
  table.querySelectorAll("tr").forEach((tr) => {
    const cells: string[] = [];
    tr.querySelectorAll("th, td").forEach((c) => cells.push(flattenCell(c.textContent ?? "")));
    if (cells.length > 0) rows.push(cells);
  });
  return toTable(rows);
}
