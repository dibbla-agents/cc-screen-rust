// Obsidian-style "live preview" for CodeMirror 6: markdown is rendered inline
// as you type — `#`/`*`/`` ` `` syntax marks are hidden and the content is
// styled — except on the line(s) holding the cursor/selection, where the raw
// markdown is revealed so it stays editable. This is exactly how Obsidian (also
// a CM6 app) behaves.
//
// The decoration logic is split into a PURE function, `computeDecorations`,
// that maps an EditorState to a list of plain `DecoSpec` descriptors. The view
// plugin below turns those into real CodeMirror decorations. Keeping the logic
// pure makes it unit-testable without a live editor view (see
// livePreview.test.ts) — the heart of the feature is verified in isolation.

import type { SyntaxNode } from "@lezer/common";
import { HighlightStyle, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { EditorState, Prec, StateField, type Extension, type Range } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { writeClipboard } from "../util";
import { noteUserCopy } from "../osc52Bus";
import { htmlTableToMarkdown, tsvToTable } from "./tableClipboard";

// A DecoSpec is a plain description of one decoration. `type`:
//   - "replace": hide the range entirely (syntax marks off the cursor line)
//   - "bullet":  replace the range with a "•" widget (unordered list markers)
//   - "mark":    add a CSS class to the range (inline styling: bold, code, …)
//   - "line":    add a CSS class to the whole line (headings, blockquotes, …)
//   - "table":   replace a whole GFM table block with a rendered <table> widget
//   - "copybtn": float a "Copy" button over a code block (carries the code text)
//   - "checkbox": replace a `[ ]`/`[x]` task marker with a clickable checkbox
export type DecoType = "replace" | "bullet" | "mark" | "line" | "table" | "copybtn" | "checkbox";

// One column's alignment, parsed from a GFM delimiter row (`:--`, `--:`, `:-:`).
export type TableAlign = "left" | "right" | "center" | null;

// The parsed contents of a GFM table — enough for the widget to render a real
// <table>. Kept out of computeDecorations' DOM concerns so the logic stays pure.
export interface TableData {
  header: string[];
  align: TableAlign[];
  body: string[][];
}

// A TableSlice describes WHICH contiguous run of a GFM table's lines one
// "table" spec renders (proposal 0085 Part B). Off the cursor a table is one
// slice covering the whole block (`whole: true`) — byte-identical to the
// pre-0085 single spec. With the caret inside, the row(s) being edited fall
// through as raw source and each maximal run of untouched lines gets its own
// slice, so the table keeps looking like a table while you edit one cell.
export interface TableSlice {
  // Does this run include the header line (and therefore its alignment
  // delimiter — the two always reveal as a unit, Part B2)?
  header: boolean;
  // Half-open window into `TableData.body` this run renders.
  bodyFrom: number;
  bodyTo: number;
  // The whole table block's document range. The Copy button copies exactly
  // these bytes (Part C2, verbatim), and per-cell offsets are resolved against
  // `blockFrom` so every slice shares one coordinate system.
  blockFrom: number;
  blockTo: number;
  // The first rendered run of this table — it carries the Copy button.
  first: boolean;
  // True when the run IS the entire table (nothing revealed).
  whole: boolean;
}

export interface DecoSpec {
  from: number;
  to: number;
  type: DecoType;
  cls?: string; // for "mark"/"line"
  table?: TableData; // for "table": the whole block's parsed contents
  slice?: TableSlice; // for "table": which run of it this spec renders
  text?: string; // for "copybtn": the code-block contents to copy
  checked?: boolean; // for "checkbox": the task marker's state ([x] → true)
}

// Heading levels map to CSS classes cm-md-h1..h6.
const HEADING: Record<string, number> = {
  ATXHeading1: 1,
  ATXHeading2: 2,
  ATXHeading3: 3,
  ATXHeading4: 4,
  ATXHeading5: 5,
  ATXHeading6: 6,
  SetextHeading1: 1,
  SetextHeading2: 2,
};

// Inline content nodes that get a styling class (always applied, even on the
// active line — Obsidian keeps bold bold while revealing the `**`).
const INLINE_STYLE: Record<string, string> = {
  StrongEmphasis: "cm-md-strong",
  Emphasis: "cm-md-em",
  Strikethrough: "cm-md-strike",
  InlineCode: "cm-md-code",
  Link: "cm-md-link",
};

// Syntax-mark nodes hidden off the active line. `URL` is handled explicitly in
// computeDecorations (not here): it is redundant syntax only inside `[text](url)`
// / `![alt](url)`, where the link text stays visible. A bare GFM autolink or an
// `<…>` autolink has no other text — the URL *is* the link — so it must stay
// visible. See the `name === "URL"` branch below.
const HIDE_MARKS = new Set([
  "HeaderMark",
  "EmphasisMark",
  "StrikethroughMark",
  "CodeMark",
  "QuoteMark",
  "LinkMark",
]);

// splitRow splits one GFM table row into trimmed cell strings, honouring the
// optional leading/trailing pipe and backslash-escaped pipes inside a cell.
function splitRow(line: string): string[] {
  let s = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && i + 1 < s.length) {
      cur += s[i] + s[i + 1];
      i++;
      continue;
    }
    if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
      continue;
    }
    cur += s[i];
  }
  cells.push(cur.trim());
  return cells;
}

// parseTableSource turns the raw lines of a GFM table (the parser already
// validated it as a Table, so line 2 is the alignment delimiter) into TableData.
// Source-based rather than tree-based so it doesn't depend on the exact lezer
// node shape; returns null if it doesn't look like a table after all.
export function parseTableSource(src: string): TableData | null {
  const lines = src.split("\n").filter((l) => l.trim().length > 0);
  if (lines.length < 2) return null;
  const header = splitRow(lines[0]);
  const align: TableAlign[] = splitRow(lines[1]).map((seg) => {
    const l = seg.startsWith(":");
    const r = seg.endsWith(":");
    return l && r ? "center" : r ? "right" : l ? "left" : null;
  });
  const body = lines.slice(2).map(splitRow);
  return { header, align, body };
}

// tableLines returns the non-blank lines of a table block together with their
// offset inside `src` — the very lines parseTableSource works from, so a row
// index means the same thing in both. Offsets are preserved (no trimming), so
// they can be turned into document positions by adding the block's `from`.
function tableLines(src: string): { text: string; start: number }[] {
  const out: { text: string; start: number }[] = [];
  let pos = 0;
  for (const text of src.split("\n")) {
    if (text.trim().length > 0) out.push({ text, start: pos });
    pos += text.length + 1;
  }
  return out;
}

// rowCellRanges returns the [from,to) offsets *within `line`* of each cell,
// mirroring splitRow's rules exactly (optional leading/trailing pipe,
// backslash-escaped pipes) but keeping positions instead of trimming them away.
// This is what makes a click land on the character the user aimed at even in a
// cell containing `\|`.
function rowCellRanges(line: string): { from: number; to: number }[] {
  let a = 0;
  let b = line.length;
  while (a < b && /\s/.test(line[a])) a++;
  while (b > a && /\s/.test(line[b - 1])) b--;
  if (a < b && line[a] === "|") a++;
  if (b > a && line[b - 1] === "|") b--;
  const out: { from: number; to: number }[] = [];
  let start = a;
  for (let i = a; i < b; i++) {
    if (line[i] === "\\" && i + 1 < b) {
      i++;
      continue;
    }
    if (line[i] === "|") {
      out.push({ from: start, to: i });
      start = i + 1;
    }
  }
  out.push({ from: start, to: b });
  return out;
}

// tableRowCells gives every cell of one table row as a [from,to) range inside
// `src`. `row` −1 = the header line, `row` ≥ 0 = body row `row` (the alignment
// delimiter, line 2, is never addressable). Null when the row doesn't exist.
function tableRowCells(src: string, row: number): { from: number; to: number }[] | null {
  const lines = tableLines(src);
  const idx = row < 0 ? 0 : row + 2;
  if (row < -1 || idx >= lines.length) return null;
  const line = lines[idx];
  return rowCellRanges(line.text).map((r) => ({
    from: line.start + r.from,
    to: line.start + r.to,
  }));
}

// cellSourceOffset returns the offset *within src* (the raw source of a whole
// GFM table block) of the first content character of cell (row, col); row −1 =
// header, row ≥ 0 = body. Null if the cell doesn't exist. An all-whitespace
// cell resolves to its end, i.e. the caret parks just before the closing pipe.
export function cellSourceOffset(src: string, row: number, col: number): number | null {
  const cells = tableRowCells(src, row);
  if (!cells || col < 0 || col >= cells.length) return null;
  const { from, to } = cells[col];
  let p = from;
  while (p < to && /\s/.test(src[p])) p++;
  return p;
}

// serializeTable renders TableData back to GFM source: one space of padding
// inside every pipe, columns padded to the widest cell, and an alignment row
// derived from `align` (null → ---, left → :--, right → --:, center → :-:).
// Ragged rows are padded with empty cells so the output always parses back to a
// rectangular table. Bare pipes inside a cell are escaped (an already-escaped
// `\|` is left alone), and internal newlines collapse to a space — the two
// things that would otherwise break the row.
//
// It is the single place the formatting rules live. In v1 only the paste
// converter emits it: nothing re-serializes bytes the user typed (open
// question 1 — auto-format on exit was deliberately declined).
export function serializeTable(data: TableData): string {
  let cols = Math.max(data.header.length, data.align.length, 1);
  for (const r of data.body) cols = Math.max(cols, r.length);

  const esc = (c: string): string =>
    c
      .replace(/\r?\n/g, " ")
      .replace(/\\\||\|/g, (m) => (m === "|" ? "\\|" : m))
      .trim();
  const pad = (row: string[]): string[] => {
    const out: string[] = [];
    for (let i = 0; i < cols; i++) out.push(esc(row[i] ?? ""));
    return out;
  };

  const header = pad(data.header);
  const body = data.body.map(pad);
  const width: number[] = [];
  for (let i = 0; i < cols; i++) {
    // 3 is the narrowest a delimiter segment can be and still round-trip its
    // alignment (`:-:` needs all three characters).
    let w = Math.max(3, header[i].length);
    for (const r of body) w = Math.max(w, r[i].length);
    width.push(w);
  }

  const row = (cells: string[]): string =>
    "| " + cells.map((c, i) => c.padEnd(width[i])).join(" | ") + " |";
  const delim = width.map((w, i) => {
    switch (data.align[i] ?? null) {
      case "left":
        return ":" + "-".repeat(w - 1);
      case "right":
        return "-".repeat(w - 1) + ":";
      case "center":
        return ":" + "-".repeat(w - 2) + ":";
      default:
        return "-".repeat(w);
    }
  });
  return [row(header), row(delim), ...body.map(row)].join("\n");
}

// toggleTaskAt flips a GFM task-list checkbox at the line containing `pos`,
// rewriting only the single box character (`[ ]` ⇄ `[x]`) so the rest of the
// document is byte-for-byte unchanged — no reflow, clean diffs for the agent,
// and the optimistic-mtime save path stays happy. Checked normalises to a
// lowercase `x`, unchecked to a space (GFM-canonical). Pure and shared by both
// render modes (the reading view passes the `<li>`'s source offset). Returns
// `changed: false` and the input untouched if `pos`'s line isn't actually a
// task item — a no-op, never a corruption.
export function toggleTaskAt(src: string, pos: number): { next: string; changed: boolean } {
  if (pos < 0 || pos > src.length) return { next: src, changed: false };
  const lineStart = src.lastIndexOf("\n", pos - 1) + 1;
  let lineEnd = src.indexOf("\n", pos);
  if (lineEnd === -1) lineEnd = src.length;
  const line = src.slice(lineStart, lineEnd);
  // Anchored to the list bullet so a literal `[x]` in prose is never matched,
  // tolerant of indentation and `-`/`*`/`+` bullets (nested + mixed lists).
  const m = /^(\s*[-*+]\s+\[)([ xX])(\])/.exec(line);
  if (!m) return { next: src, changed: false };
  const boxAbs = lineStart + m[1].length; // index of the box char in `src`
  const next = m[2].toLowerCase() === "x" ? " " : "x";
  return { next: src.slice(0, boxAbs) + next + src.slice(boxAbs + 1), changed: true };
}

// codeBlockText extracts what a code block's Copy button should put on the
// clipboard: for a fenced block, the lines between the ``` / ~~~ fences (the
// fences themselves are kept literal in the view but never copied); for an
// indented block, the block with its 4-space / tab indent stripped.
function codeBlockText(state: EditorState, name: string, from: number, to: number): string {
  const lastPos = Math.min(to, state.doc.length);
  if (name === "FencedCode") {
    const openLine = state.doc.lineAt(from).number;
    const closeLine = state.doc.lineAt(lastPos);
    const closingIsFence = /^\s*(`{3,}|~{3,})\s*$/.test(closeLine.text);
    const fromLine = openLine + 1;
    const toLine = closingIsFence ? closeLine.number - 1 : closeLine.number;
    if (toLine < fromLine) return "";
    return state.doc.sliceString(state.doc.line(fromLine).from, state.doc.line(toLine).to);
  }
  // Indented code block: drop the leading 4 spaces / tab from each line.
  return state.doc.sliceString(from, lastPos).replace(/^(\t| {1,4})/gm, "");
}

// activeLines returns the set of 1-based line numbers intersecting any part of
// the selection — those lines reveal their raw markdown.
function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const range of state.selection.ranges) {
    const a = state.doc.lineAt(range.from).number;
    const b = state.doc.lineAt(range.to).number;
    for (let n = a; n <= b; n++) lines.add(n);
  }
  return lines;
}

// computeDecorations walks the syntax tree and returns the decorations to apply,
// honouring reveal-on-cursor. Pure: depends only on `state`. The result is
// sorted by `from` (then by line-before-inline) so it can be fed straight into
// Decoration.set(..., true).
export function computeDecorations(state: EditorState): DecoSpec[] {
  const specs: DecoSpec[] = [];
  const active = activeLines(state);
  const tree = syntaxTree(state);
  // Document ranges covered by a *partial* table render. Anything the inline
  // pass emits inside one of these is shadowed by the block widget, so it is
  // dropped at the end — the revealed rows keep their inline decorations, the
  // rendered runs stay as clean as the whole-block widget always was.
  const sliceRanges: [number, number][] = [];

  const lineOf = (pos: number) => state.doc.lineAt(pos).number;
  const isActive = (pos: number) => active.has(lineOf(pos));

  tree.iterate({
    enter: (node) => {
      const name = node.name;

      // GFM table (proposal 0085 Part B). Off the cursor the whole block is one
      // rendered <table>, as before. With the caret inside, only the row being
      // edited falls through to raw source: every maximal run of untouched lines
      // still renders, so the table stays a table while you fix one cell. The
      // header line and its alignment delimiter reveal together (B2) — they
      // define the same columns, and showing one rendered above the other's raw
      // source would lie about what is being edited.
      if (name === "Table") {
        const first = state.doc.lineAt(node.from);
        const last = state.doc.lineAt(Math.min(node.to, state.doc.length));
        const blockFrom = first.from;
        const blockTo = last.to;
        const data = parseTableSource(state.doc.sliceString(blockFrom, blockTo));
        if (!data) return; // unparseable → raw source, styled inline (as before)

        const count = last.number - first.number + 1;
        const revealed: boolean[] = [];
        for (let i = 0; i < count; i++) revealed.push(active.has(first.number + i));
        if (count >= 2 && (revealed[0] || revealed[1])) revealed[0] = revealed[1] = true;
        const anyRevealed = revealed.some(Boolean);

        let i = 0;
        let firstRun = true;
        while (i < count) {
          if (revealed[i]) {
            i++;
            continue;
          }
          let j = i;
          while (j + 1 < count && !revealed[j + 1]) j++;
          const runFrom = state.doc.line(first.number + i).from;
          const runTo = state.doc.line(first.number + j).to;
          // Line 0 = header, line 1 = the alignment delimiter, line 2+k = body
          // row k. A run that starts at the header renders <thead> plus whatever
          // body rows it reaches; a body-only run renders a <tbody>-only table
          // with the same per-column alignment (it comes from `data`, which is
          // parsed from the whole block, delimiter included).
          const header = i === 0;
          const bodyFrom = header ? 0 : i - 2;
          specs.push({
            from: runFrom,
            to: runTo,
            type: "table",
            table: data,
            slice: {
              header,
              bodyFrom,
              bodyTo: Math.max(bodyFrom, j - 1),
              blockFrom,
              blockTo,
              first: firstRun,
              whole: !anyRevealed,
            },
          });
          if (anyRevealed) sliceRanges.push([runFrom, runTo]);
          firstRun = false;
          i = j + 1;
        }
        // Nothing revealed → the block is fully replaced; don't descend into the
        // cells (byte-identical to the pre-0085 output). Otherwise descend, so
        // the revealed row gets the ordinary inline pass.
        return anyRevealed ? undefined : false;
      }

      // Backslash escape (e.g. `\*` → a literal `*`): hide just the backslash so
      // the escaped character reads clean, unless its line is being edited.
      if (name === "Escape") {
        if (!isActive(node.from)) specs.push({ from: node.from, to: node.from + 1, type: "replace" });
        return;
      }

      // Block-level line styling.
      const h = HEADING[name];
      if (h) {
        const line = state.doc.lineAt(node.from);
        specs.push({ from: line.from, to: line.from, type: "line", cls: `cm-md-h${h}` });
        return;
      }
      if (name === "Blockquote") {
        // Mark each line of the quote.
        let pos = node.from;
        while (pos <= node.to) {
          const line = state.doc.lineAt(pos);
          specs.push({ from: line.from, to: line.from, type: "line", cls: "cm-md-quote" });
          if (line.to >= node.to) break;
          pos = line.to + 1;
        }
        return;
      }
      if (name === "FencedCode" || name === "CodeBlock") {
        const first = state.doc.lineAt(node.from);
        const last = state.doc.lineAt(Math.min(node.to, state.doc.length));

        // Whole-block reveal: with the cursor anywhere inside the block, show the
        // raw source (fences and all) so it stays editable — exactly how the
        // table and Obsidian behave. Otherwise we hide the ``` and surface the
        // language instead.
        let blockActive = false;
        for (let n = first.number; n <= last.number; n++) {
          if (active.has(n)) {
            blockActive = true;
            break;
          }
        }

        // Dark background on every line of the block.
        for (let n = first.number; n <= last.number; n++) {
          const line = state.doc.line(n);
          specs.push({ from: line.from, to: line.from, type: "line", cls: "cm-md-codeblock" });
        }

        if (name === "FencedCode" && !blockActive) {
          // Opening line: hide the ``` (plus any space before the info string)
          // and style the language that follows as a discrete header label.
          const fm = first.text.match(/^(\s*)(`{3,}|~{3,})([ \t]*)/);
          if (fm) {
            const ticksStart = first.from + fm[1].length;
            const infoStart = ticksStart + fm[2].length + fm[3].length;
            specs.push({ from: ticksStart, to: infoStart, type: "replace" });
            if (infoStart < first.to) {
              specs.push({ from: infoStart, to: first.to, type: "mark", cls: "cm-md-codeinfo" });
            }
          }
          // Closing line: hide the ``` and collapse the now-empty line into a
          // slim footer strip so it reads as the block's bottom padding.
          const closingIsFence = /^\s*(`{3,}|~{3,})\s*$/.test(last.text);
          if (closingIsFence && last.number > first.number) {
            const cm = last.text.match(/^(\s*)(`{3,}|~{3,})/);
            if (cm) {
              const cStart = last.from + cm[1].length;
              specs.push({ from: cStart, to: cStart + cm[2].length, type: "replace" });
            }
            specs.push({ from: last.from, to: last.from, type: "line", cls: "cm-md-codefoot" });
          }
        }

        // A "Copy" button anchored to the opening (header) line, floated
        // top-right via CSS, carrying the block's contents. Skipped for empty
        // blocks. Don't descend — code content carries no inline markup.
        const code = codeBlockText(state, name, node.from, node.to);
        if (code.length > 0) {
          specs.push({ from: first.from, to: first.from, type: "copybtn", text: code });
        }
        return false;
      }

      // Inline content styling (always applied).
      const cls = INLINE_STYLE[name];
      if (cls) {
        specs.push({ from: node.from, to: node.to, type: "mark", cls });
        return;
      }

      // A URL node is redundant syntax only as the destination of a
      // `[text](url)` / `![alt](url)` — there the link *text* stays visible, so
      // hide the URL off the active line (matching every other syntax mark).
      // Everywhere else the URL IS the visible link: a GFM bare autolink
      // (`https://…`, `www.…`, `user@host`, `mailto:…`; parent = the inline
      // container — Paragraph, an ATXHeading, a TableCell…), an `<…>` autolink
      // (parent Autolink; its `<`/`>` LinkMarks still hide off-line), or a
      // reference definition (`[r]: url`; parent LinkReference). Keep those
      // visible and style them, on the active line too — like the INLINE_STYLE
      // classes. Table cells only reach here when the table is being edited;
      // off-line the whole block is replaced by a widget (no descent).
      if (name === "URL") {
        const parent = node.node.parent?.name;
        if (parent === "Link" || parent === "Image") {
          if (!isActive(node.from)) specs.push({ from: node.from, to: node.to, type: "replace" });
        } else {
          specs.push({ from: node.from, to: node.to, type: "mark", cls: "cm-md-link" });
        }
        return;
      }

      // Syntax marks — hidden unless their line is active.
      if (HIDE_MARKS.has(name)) {
        if (isActive(node.from)) return;
        // Don't hide the fences of a code block (we keep code blocks literal,
        // just background-styled); CodeMark only hides for InlineCode.
        if (name === "CodeMark" && node.node.parent?.name !== "InlineCode") return;
        if (name === "HeaderMark") {
          // Hide the `#`/`##` plus the following spaces, so the heading text
          // starts at the margin.
          const line = state.doc.lineAt(node.from);
          let end = node.to;
          while (end < line.to && state.doc.sliceString(end, end + 1) === " ") end++;
          specs.push({ from: node.from, to: end, type: "replace" });
          return;
        }
        specs.push({ from: node.from, to: node.to, type: "replace" });
        return;
      }

      // GFM task marker (`[ ]`/`[x]`) → a clickable checkbox off the active
      // line. On the active line we leave the raw `[ ]` so it stays keyboard-
      // editable, exactly like every other syntax mark. The marker node is the
      // 3 chars `[`, box-char, `]`; the box char sits at node.from + 1.
      if (name === "TaskMarker") {
        if (isActive(node.from)) return;
        const checked = state.doc.sliceString(node.from, node.to).toLowerCase().includes("x");
        specs.push({ from: node.from, to: node.to, type: "checkbox", checked });
        // A completed item reads as done: dim + strike the whole task line.
        if (checked) {
          const line = state.doc.lineAt(node.from);
          specs.push({ from: line.from, to: line.from, type: "line", cls: "cm-md-task-done" });
        }
        return;
      }

      // Unordered list markers → a bullet glyph (off the active line). For a
      // task list item the checkbox IS the marker, so suppress the bullet there
      // (matches the reading view, which drops the bullet for task items too).
      if (name === "ListMark") {
        const grandparent = node.node.parent?.parent?.name;
        if (grandparent === "BulletList" && !isActive(node.from)) {
          if (node.node.parent?.getChild("Task")) return; // task item → no bullet
          specs.push({ from: node.from, to: node.to, type: "bullet" });
        }
        return;
      }
    },
  });

  // Drop everything the inline pass emitted inside a rendered table run (see
  // `sliceRanges`) — the block widget hides it anyway, and keeping the spec list
  // honest is what lets the tests assert "this row is uncovered".
  const kept = sliceRanges.length
    ? specs.filter(
        (s) => s.type === "table" || !sliceRanges.some(([f, t]) => s.from >= f && s.to <= t)
      )
    : specs;

  // Sort: by position, and at the same position put "line" decorations first
  // (they bind to the line start with the most-negative side).
  kept.sort((a, b) => a.from - b.from || sideOf(a) - sideOf(b));
  return kept;
}

function sideOf(s: DecoSpec): number {
  return s.type === "line" ? -2 : -1;
}

// --- View plugin: DecoSpec[] -> CodeMirror DecorationSet ---

class BulletWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const span = document.createElement("span");
    span.className = "cm-md-bullet";
    span.textContent = "•";
    return span;
  }
}

// CopyButtonWidget is the floating "Copy" button on a code block. It holds the
// code text so the click can copy without re-reading the doc. writeClipboard
// handles the HTTPS (async clipboard) vs plain-HTTP (execCommand) split, so it
// works on the tailnet's http:// deployment too.
class CopyButtonWidget extends WidgetType {
  constructor(readonly text: string) {
    super();
  }
  eq(o: CopyButtonWidget) {
    return o.text === this.text;
  }
  toDOM() {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cm-md-copy-btn";
    btn.textContent = "Copy";
    btn.setAttribute("aria-label", "Copy code block");
    // mousedown + preventDefault: stop CodeMirror moving the cursor/selection
    // into the block (which would re-render this widget), and keep the copy
    // inside the user gesture so the execCommand fallback stays allowed.
    btn.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      noteUserCopy(); // 0077 A10: don't let a session's OSC 52 swap this out
      writeClipboard(this.text)
        .then(() => {
          btn.textContent = "Copied";
          window.setTimeout(() => {
            btn.textContent = "Copy";
          }, 1200);
        })
        .catch(() => {});
    });
    return btn;
  }
  ignoreEvent() {
    return true;
  }
}

// CheckboxWidget is the clickable task-list checkbox shown off the active line
// in place of the raw `[ ]`/`[x]` marker. It carries the marker's source range
// (`from`..`to`, the 3 chars `[`, box, `]`); tapping it dispatches a one-char
// CodeMirror change at the box position, which flows through the editor's
// normal onChange → setContent → live-save path (no save code here). Like the
// table/copy widgets it toggles on `mousedown` + `preventDefault()` so the tap
// never drops the caret into the line (which would re-reveal the raw source and
// re-render the widget) and — the [0009] lesson — never blurs the editor or
// dismisses the soft keyboard on the touch PWA.
class CheckboxWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly from: number,
    readonly to: number
  ) {
    super();
  }
  eq(o: CheckboxWidget) {
    return o.checked === this.checked && o.from === this.from && o.to === this.to;
  }
  toDOM(view: EditorView) {
    const box = document.createElement("span");
    box.className = "cm-md-task" + (this.checked ? " cm-md-task-checked" : "");
    box.setAttribute("role", "checkbox");
    box.setAttribute("aria-checked", this.checked ? "true" : "false");
    box.setAttribute("aria-label", this.checked ? "Mark task incomplete" : "Mark task complete");
    box.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      // The box char sits just inside the opening bracket: from + 1.
      const boxPos = this.from + 1;
      const cur = view.state.doc.sliceString(boxPos, boxPos + 1).toLowerCase();
      const next = cur === "x" ? " " : "x";
      view.dispatch({ changes: { from: boxPos, to: boxPos + 1, insert: next } });
    });
    return box;
  }
  ignoreEvent() {
    return true;
  }
}

// appendInline renders the small subset of inline markdown that shows up in
// table cells — **strong**, `code`, and backslash escapes — into `el` as DOM
// (textContent throughout, so cell text can never inject markup).
function appendInline(el: HTMLElement, text: string): void {
  const re = /\*\*([^*]+)\*\*|`([^`]+)`|\\([\\`*_{}[\]()#+\-.!~|>])/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m.index > last) el.appendChild(document.createTextNode(text.slice(last, m.index)));
    if (m[1] !== undefined) {
      const s = document.createElement("strong");
      s.textContent = m[1];
      el.appendChild(s);
    } else if (m[2] !== undefined) {
      const c = document.createElement("code");
      c.textContent = m[2];
      el.appendChild(c);
    } else {
      el.appendChild(document.createTextNode(m[3])); // escaped char, sans backslash
    }
    last = re.lastIndex;
  }
  if (last < text.length) el.appendChild(document.createTextNode(text.slice(last)));
}

// TableWidget renders one run of a GFM table (a TableSlice — the whole block
// off the cursor, a header/body fragment while a row is being edited) as a real
// <table>. Two interactions live on it, both on `mousedown` +
// `preventDefault()` so a tap never blurs the editor or dismisses the soft
// keyboard (the [0009] rule):
//
//   • clicking a cell drops the caret at THAT cell's first content character in
//     the source (proposal 0085 Part A) — a click on the wrap but outside any
//     cell still parks it at the run's start, the pre-0085 behaviour;
//   • the Copy button (on the table's first run) puts the block's own bytes on
//     the clipboard, verbatim, through the house `writeClipboard` path (Part C).
//
// `eq` compares `from`/`blockFrom` as well as the source text: two identical
// tables at different offsets are NOT the same widget. Without that CodeMirror
// reuses the DOM — and its closure over a stale `from` — so every per-cell
// offset would be computed against the wrong base (Part A2).
export class TableWidget extends WidgetType {
  constructor(
    readonly data: TableData,
    readonly slice: TableSlice,
    readonly blockSrc: string,
    readonly from: number
  ) {
    super();
  }
  eq(o: TableWidget) {
    return (
      o.blockSrc === this.blockSrc &&
      o.from === this.from &&
      o.slice.blockFrom === this.slice.blockFrom &&
      o.slice.header === this.slice.header &&
      o.slice.bodyFrom === this.slice.bodyFrom &&
      o.slice.bodyTo === this.slice.bodyTo &&
      o.slice.first === this.slice.first &&
      o.slice.whole === this.slice.whole
    );
  }
  toDOM(view: EditorView) {
    const wrap = document.createElement("div");
    // The wrap is the positioning context for the Copy button; the inner div is
    // what scrolls, so a wide table can't push the button out of view.
    wrap.className = "cm-md-table-wrap" + (this.slice.whole ? "" : " cm-md-table-part");
    const scroll = document.createElement("div");
    scroll.className = "cm-md-table-scroll";
    const table = document.createElement("table");
    table.className = "cm-md-table";

    // data-row: −1 for a header cell, 0.. for a body row (the index into the
    // *whole* table's body, not this run's window — so every slice speaks the
    // same coordinates cellSourceOffset does).
    const cellAttrs = (el: HTMLElement, row: number, col: number) => {
      el.setAttribute("data-row", String(row));
      el.setAttribute("data-col", String(col));
    };

    if (this.slice.header) {
      const thead = document.createElement("thead");
      const htr = document.createElement("tr");
      this.data.header.forEach((cell, i) => {
        const th = document.createElement("th");
        const a = this.data.align[i];
        if (a) th.style.textAlign = a;
        cellAttrs(th, -1, i);
        appendInline(th, cell);
        htr.appendChild(th);
      });
      thead.appendChild(htr);
      table.appendChild(thead);
    }

    const tbody = document.createElement("tbody");
    for (let r = this.slice.bodyFrom; r < this.slice.bodyTo; r++) {
      const row = this.data.body[r];
      if (!row) continue;
      const tr = document.createElement("tr");
      row.forEach((cell, i) => {
        const td = document.createElement("td");
        const a = this.data.align[i];
        if (a) td.style.textAlign = a;
        cellAttrs(td, r, i);
        appendInline(td, cell);
        tr.appendChild(td);
      });
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    scroll.appendChild(table);
    wrap.appendChild(scroll);

    if (this.slice.first) wrap.appendChild(this.copyButton());

    // Tap to edit: put the cursor in the cell that was clicked, so the row
    // reveals its source with the caret already where you aimed.
    wrap.addEventListener("mousedown", (e) => {
      e.preventDefault();
      let anchor = this.from;
      const cell = (e.target as HTMLElement | null)?.closest?.("th,td") as HTMLElement | null;
      const rowAttr = cell?.getAttribute("data-row");
      const colAttr = cell?.getAttribute("data-col");
      if (rowAttr != null && colAttr != null) {
        const off = cellSourceOffset(this.blockSrc, Number(rowAttr), Number(colAttr));
        if (off != null) anchor = this.slice.blockFrom + off;
      }
      view.dispatch({ selection: { anchor } });
      // [0009]: refocus inside the gesture — iOS Safari doesn't honour the
      // preventDefault above and would otherwise drop the soft keyboard.
      view.focus();
    });
    return wrap;
  }
  // Copy the table's OWN bytes (C2): the document slice, not a
  // re-serialization, so nothing the user chose to write is silently reformatted.
  private copyButton(): HTMLButtonElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cm-md-table-copy";
    btn.textContent = "Copy";
    btn.setAttribute("aria-label", "Copy table as markdown");
    btn.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation(); // never let the wrap handler move the caret
      noteUserCopy(); // 0077 A10: don't let a session's OSC 52 swap this out
      writeClipboard(this.blockSrc)
        .then(() => {
          btn.textContent = "Copied";
          window.setTimeout(() => {
            btn.textContent = "Copy";
          }, 1200);
        })
        .catch(() => {});
    });
    return btn;
  }
  ignoreEvent() {
    return true;
  }
}

const bulletDeco = Decoration.replace({ widget: new BulletWidget() });
const hideDeco = Decoration.replace({});

// The inline/line decorations (everything except tables) — these are safe to
// provide from a view plugin. Collect into an array and let
// Decoration.set(..., true) sort by the decorations' real from/startSide — far
// safer than RangeSetBuilder, which demands the caller pre-sort by CodeMirror's
// internal side values (line decorations, marks and replacements all carry
// different sides, so a naive sort-by-`from` is rejected).
function buildInlineDecorations(state: EditorState): DecorationSet {
  const ranges: Range<Decoration>[] = [];
  for (const s of computeDecorations(state)) {
    switch (s.type) {
      case "line":
        ranges.push(Decoration.line({ class: s.cls! }).range(s.from));
        break;
      case "mark":
        ranges.push(Decoration.mark({ class: s.cls! }).range(s.from, s.to));
        break;
      case "bullet":
        ranges.push(bulletDeco.range(s.from, s.to));
        break;
      case "copybtn":
        ranges.push(
          Decoration.widget({ widget: new CopyButtonWidget(s.text ?? ""), side: 1 }).range(s.from)
        );
        break;
      case "checkbox":
        ranges.push(
          Decoration.replace({
            widget: new CheckboxWidget(!!s.checked, s.from, s.to),
            side: -1,
          }).range(s.from, s.to)
        );
        break;
      case "replace":
        ranges.push(hideDeco.range(s.from, s.to));
        break;
      case "table":
        break; // block widget — provided by the state field below, not a plugin
    }
  }
  return Decoration.set(ranges, true);
}

// The table widgets, isolated. CodeMirror forbids block / line-break-spanning
// replacing decorations from a view plugin (it throws "Block decorations may not
// be specified via plugins"), so these must come from a state field via the
// EditorView.decorations facet.
function buildTableDecorations(state: EditorState): DecorationSet {
  const ranges: Range<Decoration>[] = [];
  for (const s of computeDecorations(state)) {
    if (s.type !== "table" || !s.slice) continue;
    const sl = s.slice;
    ranges.push(
      Decoration.replace({
        widget: new TableWidget(s.table!, sl, state.sliceDoc(sl.blockFrom, sl.blockTo), s.from),
        block: true,
      }).range(s.from, s.to)
    );
  }
  return Decoration.set(ranges, true);
}

const livePreviewPlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(view: EditorView) {
      this.decorations = buildInlineDecorations(view.state);
    }
    update(u: ViewUpdate) {
      if (u.docChanged || u.selectionSet || u.viewportChanged) {
        this.decorations = buildInlineDecorations(u.state);
      }
    }
  },
  { decorations: (v) => v.decorations }
);

// Tables come through a state field (block decorations can't come from a plugin).
// Recompute when the doc or selection changes — the latter so moving the cursor
// into a table reveals its raw source, and out of it re-renders the widget.
const tableField = StateField.define<DecorationSet>({
  create: (state) => buildTableDecorations(state),
  update(deco, tr) {
    if (tr.docChanged || tr.selection) return buildTableDecorations(tr.state);
    return deco.map(tr.changes);
  },
  provide: (f) => EditorView.decorations.from(f),
});

// Theme: the visual styling for the classes the plugin emits. It mirrors the
// reading-view `.cc-prose` rules (index.css) so toggling Edit<->Read keeps one
// continuous document: editorial serif headings, warm ink, accent code/quotes.
const livePreviewTheme = EditorView.baseTheme({
  ".cm-md-h1, .cm-md-h2, .cm-md-h3, .cm-md-h4, .cm-md-h5, .cm-md-h6": {
    color: "var(--cc-ink-strong, #f4f2ea)",
    fontWeight: "600",
    lineHeight: "1.25",
    letterSpacing: "-0.01em",
  },
  ".cm-md-h1": { fontSize: "1.5em" },
  ".cm-md-h2": { fontSize: "1.3em" },
  ".cm-md-h3": { fontSize: "1.15em" },
  ".cm-md-h4": { fontSize: "1.05em" },
  ".cm-md-h5": { fontSize: "1em" },
  ".cm-md-h6": { fontSize: "0.92em", color: "var(--cc-ink-faint, #9aa6b2)" },
  ".cm-md-strong": { fontWeight: "700", color: "var(--cc-ink-strong, #f4f2ea)" },
  ".cm-md-em": { fontStyle: "italic" },
  ".cm-md-strike": { textDecoration: "line-through", opacity: "0.6" },
  ".cm-md-code": {
    fontFamily: "var(--cc-mono-font)",
    fontSize: "0.86em",
    background: "rgba(56,189,248,0.10)",
    color: "#bfe3ff",
    borderRadius: "4px",
    padding: "0.1em 0.34em",
  },
  ".cm-md-link": { color: "#7cc2ff", textDecoration: "none", borderBottom: "1px solid rgba(124,194,255,0.4)" },
  // Follow affordance: pointer cursor when Mod is held (desktop) or on a touch
  // device, where a plain tap opens the link. See the `linkClicks` handler.
  // `&` = the editor root (view.dom), where modKeyAffordance sets the class; a
  // plain `.cm-mod-held …` selector would compile to a descendant match
  // (`.ͼx .cm-mod-held …`) and never fire — buildTheme prefixes `&`-less
  // selectors with `scope + " "`.
  "&.cm-mod-held .cm-md-link": { cursor: "pointer !important" },
  "@media (pointer: coarse)": {
    ".cm-md-link": { cursor: "pointer !important" },
  },
  ".cm-md-quote": {
    borderLeft: "3px solid var(--cc-accent, #38bdf8)",
    paddingLeft: "0.9em",
    color: "var(--cc-ink-faint, #9aa6b2)",
    fontStyle: "italic",
  },
  ".cm-md-codeblock": {
    background: "#0b1118",
    fontFamily: "var(--cc-mono-font)",
    fontSize: "0.9em",
    position: "relative", // anchor the absolutely-positioned copy button
  },
  // The language label that replaces the opening ``` (e.g. "bash", "js") — kept
  // small and faint so it reads as a discrete tag on the block's header line.
  ".cm-md-codeinfo": {
    fontSize: "0.72em",
    letterSpacing: "0.04em",
    color: "var(--cc-ink-faint, #9aa6b2)",
    opacity: "0.75",
  },
  // The closing ``` line with its ticks hidden — collapse it to a slim strip so
  // it reads as the code block's bottom padding rather than a blank row.
  ".cm-md-codefoot": { fontSize: "0", lineHeight: "10px" },
  // Floated over the opening fence line's top-right corner. Stays visible (not
  // hover-only) so it works on the touch PWA; brightens on hover.
  ".cm-md-copy-btn": {
    position: "absolute",
    top: "2px",
    right: "6px",
    zIndex: "2",
    fontFamily: "var(--cc-mono-font)",
    fontSize: "0.72em",
    lineHeight: "1",
    color: "var(--cc-ink-faint, #9aa6b2)",
    background: "rgba(11,17,24,0.85)",
    border: "1px solid var(--cc-edge, #243042)",
    borderRadius: "6px",
    padding: "0.3em 0.55em",
    cursor: "pointer",
    opacity: "0.6",
  },
  ".cm-md-copy-btn:hover": {
    opacity: "1",
    color: "var(--cc-ink, #d7dade)",
    borderColor: "var(--cc-accent, #38bdf8)",
  },
  ".cm-md-bullet": { paddingRight: "0.5em", color: "var(--cc-accent, #38bdf8)" },
  // GFM task checkbox. A rounded square sized to the text; the *visual* box is
  // ~1em but the tap target is enlarged via padding (a ~28px hit area) so it's
  // easy to thumb on the phone PWA without looking heavy. Not hover-dependent.
  ".cm-md-task": {
    display: "inline-block",
    boxSizing: "content-box",
    width: "1em",
    height: "1em",
    margin: "0 0.45em 0 0",
    padding: "6px",
    // pull the padding back so the box still sits inline without bloating line height
    marginTop: "-6px",
    marginBottom: "-6px",
    verticalAlign: "middle",
    backgroundClip: "content-box",
    border: "1.5px solid var(--cc-edge, #243042)",
    borderRadius: "5px",
    cursor: "pointer",
    transition: "background-color 0.12s ease, border-color 0.12s ease",
  },
  ".cm-md-task:hover": { borderColor: "var(--cc-accent, #38bdf8)" },
  // Checked: accent fill + a crisp check glyph drawn with a rotated border.
  ".cm-md-task-checked": {
    background: "var(--cc-accent, #38bdf8)",
    borderColor: "var(--cc-accent, #38bdf8)",
    position: "relative",
  },
  ".cm-md-task-checked::after": {
    content: '""',
    position: "absolute",
    left: "50%",
    top: "46%",
    width: "0.32em",
    height: "0.6em",
    transform: "translate(-50%, -55%) rotate(45deg)",
    border: "solid var(--cc-bar, #0b1118)",
    borderWidth: "0 2px 2px 0",
  },
  // A completed task line reads as done: dimmed, with struck-through text. The
  // checkbox widget itself opts out of the strike so the box stays crisp.
  ".cm-md-task-done": { opacity: "0.55", textDecoration: "line-through" },
  ".cm-md-task-done .cm-md-task": { textDecoration: "none", opacity: "1" },
  // Rendered GFM tables (the TableWidget) — mirrors the reading view's
  // `.cc-prose table` rules (index.css) so Edit<->Read stay one document.
  ".cm-md-table-wrap": { margin: "0.9em 0", position: "relative" },
  // The scroll box is the INNER element so the absolutely-positioned Copy
  // button (anchored to the wrap) can't be scrolled out of view on a wide table.
  ".cm-md-table-scroll": { overflowX: "auto" },
  // A fragment of a table being edited (0085 B3): the runs above and below the
  // revealed row are separate <table>s, so drop the block margin to keep the
  // seam tight. Their column widths still lay out independently — accepted.
  ".cm-md-table-part": { margin: "0" },
  ".cm-md-table-part .cm-md-table": { width: "100%" },
  // Copy the table as markdown (0085 Part C). Always visible (dimmed) where
  // there is no hover — the touch PWA — and hover-revealed on desktop. The
  // padding takes the box past a 24px hit target without looking heavy.
  ".cm-md-table-copy": {
    position: "absolute",
    top: "2px",
    right: "2px",
    zIndex: "2",
    fontFamily: "var(--cc-mono-font)",
    fontSize: "0.72em",
    lineHeight: "1",
    minWidth: "24px",
    minHeight: "24px",
    color: "var(--cc-ink-faint, #9aa6b2)",
    background: "rgba(11,17,24,0.9)",
    border: "1px solid var(--cc-edge, #243042)",
    borderRadius: "6px",
    padding: "0.45em 0.6em",
    cursor: "pointer",
    opacity: "0.55",
  },
  "@media (hover: hover)": {
    ".cm-md-table-copy": { opacity: "0" },
    ".cm-md-table-wrap:hover .cm-md-table-copy, .cm-md-table-wrap:focus-within .cm-md-table-copy":
      { opacity: "0.8" },
    ".cm-md-table-copy:hover": {
      opacity: "1",
      color: "var(--cc-ink, #d7dade)",
      borderColor: "var(--cc-accent, #38bdf8)",
    },
  },
  ".cm-md-table": { borderCollapse: "collapse", fontSize: "0.95em" },
  ".cm-md-table th, .cm-md-table td": {
    border: "1px solid var(--cc-edge, #243042)",
    padding: "0.4em 0.7em",
    verticalAlign: "top",
  },
  ".cm-md-table th": { background: "rgba(127,127,127,0.08)", fontWeight: "600", textAlign: "left" },
  ".cm-md-table strong": { fontWeight: "700", color: "var(--cc-ink-strong, #f4f2ea)" },
  ".cm-md-table code": {
    fontFamily: "var(--cc-mono-font)",
    fontSize: "0.86em",
    background: "rgba(56,189,248,0.10)",
    color: "#bfe3ff",
    borderRadius: "4px",
    padding: "0.1em 0.34em",
  },
});

// CodeMirror's default highlight style underlines `tags.heading` and `tags.link`
// (and tints escapes) — fighting our clean, Obsidian-style look. We do heading
// and link styling ourselves via the decoration classes, so neutralise those
// defaults here. Added after the default style, so it wins on the shared tags.
const mdHighlight = HighlightStyle.define([
  { tag: tags.heading, textDecoration: "none" },
  { tag: tags.heading1, textDecoration: "none" },
  { tag: tags.heading2, textDecoration: "none" },
  { tag: tags.heading3, textDecoration: "none" },
  { tag: tags.heading4, textDecoration: "none" },
  { tag: tags.heading5, textDecoration: "none" },
  { tag: tags.heading6, textDecoration: "none" },
  { tag: tags.link, textDecoration: "none", color: "inherit" },
  { tag: tags.escape, color: "inherit" },
]);

// --- Clickable links (follow-on to 0052) -------------------------------------

// linkNodeUrl finds the destination-URL text of any link touching `pos`. It
// climbs from the resolved node to the nearest link container: a `[text](url)`
// / `![alt](url)` (name Link/Image — the URL is a child, and the click may land
// on the visible *text*, not the hidden URL), an `<…>` autolink (Autolink), or
// a bare GFM autolink where the `URL` node itself is what was clicked. Returns
// the raw source text of the `URL` node, or null if `pos` isn't in a link.
export function linkNodeUrl(state: EditorState, pos: number): string | null {
  const tree = syntaxTree(state);
  // Probe both sides — a click at a boundary can resolve to the adjacent node.
  for (const side of [-1, 1] as const) {
    let n: SyntaxNode | null = tree.resolveInner(pos, side);
    for (; n; n = n.parent) {
      if (n.name === "URL") return state.sliceDoc(n.from, n.to);
      if (n.name === "Link" || n.name === "Image" || n.name === "Autolink") {
        const url = n.getChild("URL");
        if (url) return state.sliceDoc(url.from, url.to);
      }
    }
  }
  return null;
}

// hrefFromUrlText turns a `URL` node's raw text into a followable href, matching
// how remark-gfm autolinks in the reading view: keep an explicit scheme
// (`http:`, `https:`, `mailto:`, `xmpp:`…), turn a bare `user@host` into
// `mailto:`, and default a schemeless host (`www.example.com`) to `https://`.
export function hrefFromUrlText(raw: string): string {
  const t = raw.trim();
  if (/^[a-z][a-z0-9+.-]*:/i.test(t)) return t; // already has a scheme
  if (/^[^\s@]+@[^\s@]+$/.test(t)) return "mailto:" + t; // bare email
  return "https://" + t; // www.… / schemeless host
}

// linkClicks makes live-preview links followable without breaking editing. The
// gesture (chosen with the user): Mod(Cmd/Ctrl)+click always opens; on a touch
// device a plain tap opens too, but only when the link's line isn't already the
// active (being-edited) line — so tapping the surrounding text first to reveal
// the raw source still lets you edit the URL. Plain mouse click (no modifier)
// always places the caret. Opening uses a fresh tab with noopener so the PWA is
// never navigated away. Like the widget handlers we act on `mousedown` +
// preventDefault (the [0009] lesson: never blur the editor / drop the caret /
// dismiss the soft keyboard), and only when we're actually going to open —
// otherwise we return and let CodeMirror handle the click normally.
const coarsePointer = (): boolean =>
  typeof window !== "undefined" && !!window.matchMedia?.("(pointer: coarse)").matches;

function openLinkFromEvent(view: EditorView, e: MouseEvent): boolean {
  const el = (e.target as HTMLElement | null)?.closest?.(".cm-md-link");
  if (!el) return false;
  const pos =
    view.posAtCoords({ x: e.clientX, y: e.clientY }) ?? view.posAtDOM(el as HTMLElement);
  if (pos == null) return false;
  const raw = linkNodeUrl(view.state, pos);
  if (raw == null) return false;

  const mod = e.metaKey || e.ctrlKey;
  const sel = view.state.selection.main;
  const clickLine = view.state.doc.lineAt(pos).number;
  const lineActive =
    view.state.doc.lineAt(sel.head).number === clickLine ||
    view.state.doc.lineAt(sel.anchor).number === clickLine;
  const shouldOpen = mod || (coarsePointer() && !lineActive);
  if (!shouldOpen) return false;

  e.preventDefault();
  e.stopPropagation();
  window.open(hrefFromUrlText(raw), "_blank", "noopener,noreferrer");
  return true;
}

const linkClicks = EditorView.domEventHandlers({
  mousedown: (e, view) => openLinkFromEvent(view, e),
});

// modKeyAffordance toggles a `cm-mod-held` class on the editor while Cmd/Ctrl is
// held, so the CSS can show a pointer cursor over links (the "follow" gesture is
// desktop-only and modifier-gated). Listeners live on `window` (capture) rather
// than the editor DOM: a lone modifier keydown/keyup must register even when the
// editor isn't the focus target, and reading `metaKey`/`ctrlKey` off *any* key
// event tracks the state without caring which key it was. It never calls
// preventDefault, so typing is untouched. Cleared on window blur (tab switch) so
// the class can't get stuck if the keyup lands in another tab.
const modKeyAffordance = ViewPlugin.fromClass(
  class {
    sync = (e: KeyboardEvent) => {
      this.view.dom.classList.toggle("cm-mod-held", e.metaKey || e.ctrlKey);
    };
    clear = () => this.view.dom.classList.remove("cm-mod-held");
    constructor(readonly view: EditorView) {
      window.addEventListener("keydown", this.sync, true);
      window.addEventListener("keyup", this.sync, true);
      window.addEventListener("blur", this.clear);
    }
    destroy() {
      window.removeEventListener("keydown", this.sync, true);
      window.removeEventListener("keyup", this.sync, true);
      window.removeEventListener("blur", this.clear);
    }
  }
);

// --- Table editing: Tab walks the cells, paste understands a spreadsheet -----

// tableBlockAt resolves the whole-line range of the GFM table containing `pos`,
// or null when `pos` isn't inside one.
function tableBlockAt(state: EditorState, pos: number): { from: number; to: number } | null {
  const tree = syntaxTree(state);
  for (const side of [-1, 1] as const) {
    let n: SyntaxNode | null = tree.resolveInner(pos, side);
    for (; n; n = n.parent) {
      if (n.name === "Table") {
        const first = state.doc.lineAt(n.from);
        const last = state.doc.lineAt(Math.min(n.to, state.doc.length));
        return { from: first.from, to: last.to };
      }
    }
  }
  return null;
}

// tableCellTarget answers where Tab (`forward`) / Shift-Tab should put the
// caret from `pos` (proposal 0085 B5):
//
//   • null    — `pos` isn't on an editable table row (the alignment delimiter
//               counts as "not a row"); the key binding must fall through, so
//               Tab outside a table behaves exactly as it always did.
//   • "edge"  — in a table, but there is no next/previous cell (the last cell of
//               the last row, the first cell of the header). Consume the key:
//               growing the table is a non-goal.
//   • number  — the document offset of the target cell's first content char.
export function tableCellTarget(
  state: EditorState,
  pos: number,
  forward: boolean
): number | "edge" | null {
  const block = tableBlockAt(state, pos);
  if (!block) return null;
  const src = state.sliceDoc(block.from, block.to);
  const lines = tableLines(src);
  if (lines.length < 2) return null;
  const li = lines.findIndex((l) => l.start === state.doc.lineAt(pos).from - block.from);
  if (li < 0 || li === 1) return null; // off the table, or on the delimiter line
  const row = li === 0 ? -1 : li - 2;
  const lastRow = lines.length - 3; // body rows are lines 2..end
  const cells = tableRowCells(src, row);
  if (!cells || cells.length === 0) return null;

  const rel = pos - block.from;
  let col = cells.findIndex((c) => rel >= c.from && rel <= c.to);
  if (col < 0) col = rel < cells[0].from ? 0 : cells.length - 1;

  let tRow = row;
  let tCol = col + (forward ? 1 : -1);
  if (tCol < 0 || tCol >= cells.length) {
    tRow = row + (forward ? 1 : -1);
    if (tRow < -1 || tRow > lastRow) return "edge";
    const next = tableRowCells(src, tRow);
    if (!next || next.length === 0) return "edge";
    tCol = forward ? 0 : next.length - 1;
  }
  const off = cellSourceOffset(src, tRow, tCol);
  return off == null ? "edge" : block.from + off;
}

function tableTab(view: EditorView, forward: boolean): boolean {
  const target = tableCellTarget(view.state, view.state.selection.main.head, forward);
  if (target == null) return false;
  if (target === "edge") return true;
  view.dispatch({ selection: { anchor: target }, scrollIntoView: true });
  return true;
}

// Tab / Shift-Tab move cell-to-cell, but ONLY on a table row — everywhere else
// the commands return false and the (absent) default Tab behaviour is
// untouched. Prec.high so it sits above basicSetup, which binds no Tab at all.
const tableKeymap = Prec.high(
  keymap.of([
    { key: "Tab", run: (v) => tableTab(v, true) },
    { key: "Shift-Tab", run: (v) => tableTab(v, false) },
  ])
);

// tablePasteInsert builds the text one paste inserts, padding it with blank
// lines when the insertion point isn't already at a block boundary — a table
// glued to a paragraph doesn't parse as a table.
export function tablePasteInsert(state: EditorState, from: number, to: number, md: string): string {
  const startLine = state.doc.lineAt(from);
  const endLine = state.doc.lineAt(to);
  const before = state.sliceDoc(startLine.from, from);
  const after = state.sliceDoc(to, endLine.to);
  let prefix = "";
  if (before.trim() !== "") prefix = "\n\n";
  else if (startLine.number > 1 && state.doc.line(startLine.number - 1).text.trim() !== "")
    prefix = "\n";
  let suffix = "";
  if (after.trim() !== "") suffix = "\n\n";
  else if (endLine.number < state.doc.lines && state.doc.line(endLine.number + 1).text.trim() !== "")
    suffix = "\n";
  return prefix + md + suffix;
}

// Paste a spreadsheet block as a GFM table (proposal 0085 Part D). Detection is
// deliberately conservative — Excel/Sheets/Numbers `text/html` carrying exactly
// one <table>, or classic all-tabbed TSV — because a false negative pastes
// plain text (yesterday's behaviour) while a false positive mangles the user's
// paste. Only the event's own `clipboardData` is read (never the async
// clipboard API, which would prompt), and the conversion is ONE transaction, so
// a single undo restores the document as if the paste never happened.
const tablePaste = EditorView.domEventHandlers({
  paste: (e, view) => {
    if (view.state.readOnly) return false;
    const cd = e.clipboardData;
    if (!cd) return false;
    const html = cd.getData("text/html");
    let data = html ? htmlTableToMarkdown(html) : null;
    if (!data) {
      const text = cd.getData("text/plain");
      data = text ? tsvToTable(text) : null;
    }
    if (!data) return false;
    const { from, to } = view.state.selection.main;
    const insert = tablePasteInsert(view.state, from, to, serializeTable(data));
    e.preventDefault();
    view.dispatch({
      changes: { from, to, insert },
      selection: { anchor: from + insert.length },
      userEvent: "input.paste",
      scrollIntoView: true,
    });
    return true;
  },
});

// livePreview is the full extension: the inline-decoration plugin, the table
// state field, the theme, the clickable-link handler + its cursor affordance,
// the table cell keymap + spreadsheet paste, and the highlight-style override
// that strips the default heading/link underline.
export function livePreview(): Extension {
  return [
    livePreviewPlugin,
    tableField,
    livePreviewTheme,
    linkClicks,
    modKeyAffordance,
    tableKeymap,
    tablePaste,
    syntaxHighlighting(mdHighlight),
  ];
}
