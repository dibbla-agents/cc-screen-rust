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

import { HighlightStyle, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { EditorState, StateField, type Extension, type Range } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { writeClipboard } from "../util";

// A DecoSpec is a plain description of one decoration. `type`:
//   - "replace": hide the range entirely (syntax marks off the cursor line)
//   - "bullet":  replace the range with a "•" widget (unordered list markers)
//   - "mark":    add a CSS class to the range (inline styling: bold, code, …)
//   - "line":    add a CSS class to the whole line (headings, blockquotes, …)
//   - "table":   replace a whole GFM table block with a rendered <table> widget
//   - "copybtn": float a "Copy" button over a code block (carries the code text)
export type DecoType = "replace" | "bullet" | "mark" | "line" | "table" | "copybtn";

// One column's alignment, parsed from a GFM delimiter row (`:--`, `--:`, `:-:`).
export type TableAlign = "left" | "right" | "center" | null;

// The parsed contents of a GFM table — enough for the widget to render a real
// <table>. Kept out of computeDecorations' DOM concerns so the logic stays pure.
export interface TableData {
  header: string[];
  align: TableAlign[];
  body: string[][];
}

export interface DecoSpec {
  from: number;
  to: number;
  type: DecoType;
  cls?: string; // for "mark"/"line"
  table?: TableData; // for "table"
  text?: string; // for "copybtn": the code-block contents to copy
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

// Syntax-mark nodes hidden off the active line.
const HIDE_MARKS = new Set([
  "HeaderMark",
  "EmphasisMark",
  "StrikethroughMark",
  "CodeMark",
  "QuoteMark",
  "LinkMark",
  "URL",
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

  const lineOf = (pos: number) => state.doc.lineAt(pos).number;
  const isActive = (pos: number) => active.has(lineOf(pos));

  tree.iterate({
    enter: (node) => {
      const name = node.name;

      // GFM table: off the cursor, swap the whole block for a rendered <table>.
      // When the selection is inside it, fall through so the raw source shows and
      // stays editable — exactly Obsidian's live-preview behaviour.
      if (name === "Table") {
        const first = state.doc.lineAt(node.from);
        const last = state.doc.lineAt(node.to);
        let tableActive = false;
        for (let n = first.number; n <= last.number; n++) {
          if (active.has(n)) {
            tableActive = true;
            break;
          }
        }
        if (!tableActive) {
          const data = parseTableSource(state.doc.sliceString(first.from, last.to));
          if (data) {
            specs.push({ from: first.from, to: last.to, type: "table", table: data });
            return false; // replace the whole block — don't descend into cells
          }
        }
        return; // editing it (or unparseable) → show source, style inline content
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

      // Unordered list markers → a bullet glyph (off the active line).
      if (name === "ListMark") {
        const grandparent = node.node.parent?.parent?.name;
        if (grandparent === "BulletList" && !isActive(node.from)) {
          specs.push({ from: node.from, to: node.to, type: "bullet" });
        }
        return;
      }
    },
  });

  // Sort: by position, and at the same position put "line" decorations first
  // (they bind to the line start with the most-negative side).
  specs.sort((a, b) => a.from - b.from || sideOf(a) - sideOf(b));
  return specs;
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

// TableWidget renders parsed TableData as a real <table>. Tapping it drops the
// cursor into the table source (`from`) so the raw markdown reveals for editing.
class TableWidget extends WidgetType {
  constructor(
    readonly data: TableData,
    readonly src: string,
    readonly from: number
  ) {
    super();
  }
  eq(o: TableWidget) {
    return o.src === this.src;
  }
  toDOM(view: EditorView) {
    const wrap = document.createElement("div");
    wrap.className = "cm-md-table-wrap";
    const table = document.createElement("table");
    table.className = "cm-md-table";

    const thead = document.createElement("thead");
    const htr = document.createElement("tr");
    this.data.header.forEach((cell, i) => {
      const th = document.createElement("th");
      const a = this.data.align[i];
      if (a) th.style.textAlign = a;
      appendInline(th, cell);
      htr.appendChild(th);
    });
    thead.appendChild(htr);
    table.appendChild(thead);

    const tbody = document.createElement("tbody");
    this.data.body.forEach((row) => {
      const tr = document.createElement("tr");
      row.forEach((cell, i) => {
        const td = document.createElement("td");
        const a = this.data.align[i];
        if (a) td.style.textAlign = a;
        appendInline(td, cell);
        tr.appendChild(td);
      });
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);
    wrap.appendChild(table);

    // Tap to edit: put the cursor at the table start so the source reveals.
    wrap.addEventListener("mousedown", (e) => {
      e.preventDefault();
      view.dispatch({ selection: { anchor: this.from } });
      view.focus();
    });
    return wrap;
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
    if (s.type !== "table") continue;
    ranges.push(
      Decoration.replace({
        widget: new TableWidget(s.table!, state.sliceDoc(s.from, s.to), s.from),
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
  // Rendered GFM tables (the TableWidget) — mirrors the reading view's
  // `.cc-prose table` rules (index.css) so Edit<->Read stay one document.
  ".cm-md-table-wrap": { margin: "0.9em 0", overflowX: "auto" },
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

// livePreview is the full extension: the inline-decoration plugin, the table
// state field, the theme, and the highlight-style override that strips the
// default heading/link underline.
export function livePreview(): Extension {
  return [livePreviewPlugin, tableField, livePreviewTheme, syntaxHighlighting(mdHighlight)];
}
