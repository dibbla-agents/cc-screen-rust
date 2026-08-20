import { describe, it, expect, beforeAll } from "vitest";
import { EditorState, EditorSelection } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { markdownLanguage } from "@codemirror/lang-markdown";
import {
  cellSourceOffset,
  computeDecorations,
  parseTableSource,
  serializeTable,
  tableCellTarget,
  tablePasteInsert,
  TableWidget,
  toggleTaskAt,
  linkNodeUrl,
  hrefFromUrlText,
  livePreview,
  type DecoSpec,
} from "./livePreview";

// Build an EditorState for `doc` with the cursor at `cursor` (default 0). The
// markdown language is what gives syntaxTree() a parse to walk.
function stateFor(doc: string, cursor = 0): EditorState {
  return EditorState.create({
    doc,
    selection: EditorSelection.cursor(cursor),
    extensions: [markdownLanguage],
  });
}

// Specs overlapping [from,to) of a given type, for terse assertions.
function specsIn(specs: DecoSpec[], type: string, from: number, to: number): DecoSpec[] {
  return specs.filter((s) => s.type === type && s.from < to && s.to > from);
}

describe("computeDecorations", () => {
  it("hides heading marks and styles the heading line when the cursor is elsewhere", () => {
    const doc = "# Title\n\nbody text here\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1)); // cursor in body
    // The `# ` (positions 0..2) is hidden.
    const hidden = specsIn(specs, "replace", 0, 2);
    expect(hidden.length).toBe(1);
    expect(hidden[0].from).toBe(0);
    expect(hidden[0].to).toBe(2); // includes the space after '#'
    // The heading line carries the h1 class.
    const line = specs.find((s) => s.type === "line" && s.cls === "cm-md-h1");
    expect(line).toBeTruthy();
    expect(line!.from).toBe(0);
  });

  it("reveals the heading mark when the cursor is on the heading line", () => {
    const doc = "# Title\n\nbody\n";
    const specs = computeDecorations(stateFor(doc, 3)); // cursor inside "Title"
    expect(specsIn(specs, "replace", 0, 2).length).toBe(0); // not hidden
    // Styling still applies even while revealed.
    expect(specs.some((s) => s.type === "line" && s.cls === "cm-md-h1")).toBe(true);
  });

  it("styles bold and hides its ** markers off the cursor line", () => {
    const doc = "a **bold** b\n\nx\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    // StrongEmphasis spans positions 2..10 ("**bold**").
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-strong")).toBe(true);
    // Two EmphasisMark replaces: the opening ** (2..4) and closing ** (8..10).
    expect(specsIn(specs, "replace", 2, 4).length).toBe(1);
    expect(specsIn(specs, "replace", 8, 10).length).toBe(1);
  });

  it("styles inline code and hides its backticks", () => {
    const doc = "use `code` now\n\ny\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-code")).toBe(true);
    // Backticks at 4 and 9 hidden.
    expect(specsIn(specs, "replace", 4, 5).length).toBe(1);
    expect(specsIn(specs, "replace", 9, 10).length).toBe(1);
  });

  it("turns an unordered list marker into a bullet widget off the cursor line", () => {
    const doc = "- one\n- two\n\nz\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const bullets = specs.filter((s) => s.type === "bullet");
    expect(bullets.length).toBe(2);
    // First marker at position 0.
    expect(bullets[0].from).toBe(0);
  });

  it("hides link marks and the URL but styles the link text", () => {
    const doc = "see [text](http://x.com) ok\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-link")).toBe(true);
    // The URL (inside the parens) is replaced/hidden.
    const urlStart = doc.indexOf("http://");
    expect(specsIn(specs, "replace", urlStart, urlStart + 5).length).toBeGreaterThan(0);
  });

  // --- Proposal 0052: bare autolinks/emails must stay visible off-line ---

  // A bare URL/email/www/mailto link is not redundant syntax — it IS the link,
  // so it must never be hidden and it must carry the link style, cursor
  // anywhere. helper: assert `frag` (first occurrence in `doc`) is never
  // `replace`d and is covered by a `cm-md-link` mark.
  function expectVisibleLink(doc: string, frag: string, cursor: number) {
    const at = doc.indexOf(frag);
    expect(at).toBeGreaterThanOrEqual(0);
    const specs = computeDecorations(stateFor(doc, cursor));
    expect(specsIn(specs, "replace", at, at + frag.length).length).toBe(0);
    const marks = specsIn(specs, "mark", at, at + frag.length).filter((s) => s.cls === "cm-md-link");
    expect(marks.length).toBeGreaterThan(0);
    return specs;
  }

  it("keeps a bare https URL visible and link-styled off the cursor line", () => {
    // cursor on the trailing blank/other line, not the URL's line
    expectVisibleLink("see https://example.com ok\n\nq\n", "https://example.com", 25);
  });

  it("keeps a bare email address visible and link-styled off the cursor line", () => {
    expectVisibleLink("mail erik@dibbla.com now\n\nq\n", "erik@dibbla.com", 26);
  });

  it("keeps a bare www. link visible and link-styled off the cursor line", () => {
    expectVisibleLink("at www.example.com here\n\nq\n", "www.example.com", 25);
  });

  it("keeps a mailto: autolink visible and link-styled off the cursor line", () => {
    expectVisibleLink("write mailto:a@b.com please\n\nq\n", "mailto:a@b.com", 30);
  });

  it("shows the URL of an <angle> autolink and hides only its brackets", () => {
    const doc = "go <https://example.com> now\n\nq\n";
    const url = "https://example.com";
    const at = doc.indexOf(url);
    const specs = computeDecorations(stateFor(doc, doc.length - 1)); // cursor in 'q'
    // The URL itself is not replaced and is link-styled.
    expect(specsIn(specs, "replace", at, at + url.length).length).toBe(0);
    expect(specsIn(specs, "mark", at, at + url.length).some((s) => s.cls === "cm-md-link")).toBe(true);
    // The `<` (just before) and `>` (just after) are each hidden.
    expect(specsIn(specs, "replace", at - 1, at).length).toBe(1); // '<'
    expect(specsIn(specs, "replace", at + url.length, at + url.length + 1).length).toBe(1); // '>'
  });

  it("keeps a bare URL visible and link-styled even on its own cursor line", () => {
    // styling is not cursor-dependent for bare autolinks (like bold stays bold)
    const doc = "see https://example.com ok\n";
    expectVisibleLink(doc, "https://example.com", doc.indexOf("https") + 3);
  });

  it("keeps a bare URL visible inside a blockquote and inside a list item", () => {
    // blockquote (parent Paragraph under the quote) and list item (Paragraph)
    expectVisibleLink("x\n\n> quote https://a.example here\n", "https://a.example", 0);
    expectVisibleLink("x\n\n- item https://b.example here\n", "https://b.example", 0);
  });

  it("still hides the URL of an inline [text](url) link off the cursor line", () => {
    // regression: the [text](url) form is unchanged — URL hidden, text styled.
    const doc = "see [text](http://x.com) ok\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const urlStart = doc.indexOf("http://");
    expect(specsIn(specs, "replace", urlStart, urlStart + 5).length).toBeGreaterThan(0);
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-link")).toBe(true);
  });

  it("still hides the URL of an ![alt](url) image off the cursor line", () => {
    const doc = "img ![alt](http://x.com/i.png) ok\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const urlStart = doc.indexOf("http://");
    expect(specsIn(specs, "replace", urlStart, urlStart + 5).length).toBeGreaterThan(0);
  });

  it("reveals the URL of an inline link when its own line holds the cursor", () => {
    const doc = "see [text](http://x.com) ok\n";
    const urlStart = doc.indexOf("http://");
    const specs = computeDecorations(stateFor(doc, urlStart)); // cursor on the link line
    expect(specsIn(specs, "replace", urlStart, urlStart + 5).length).toBe(0);
  });

  it("keeps a reference-definition URL ([r]: url) visible and link-styled", () => {
    // parent LinkReference — not Link/Image, so the definition's URL is the
    // payload and must not ghost out (consistent with the autolink fix).
    expectVisibleLink("use [r][r]\n\n[r]: https://ref.example\n", "https://ref.example", 0);
  });

  it("backgrounds fenced code lines and hides the fences off the cursor", () => {
    const doc = "```js\nlet a=1;\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1)); // cursor in 'q'
    const cb = specs.filter((s) => s.type === "line" && s.cls === "cm-md-codeblock");
    expect(cb.length).toBeGreaterThanOrEqual(2); // every block line gets the bg
    // The opening ``` (0..3) is hidden, and "js" (3..5) becomes a discrete label.
    expect(specsIn(specs, "replace", 0, 3).length).toBe(1);
    expect(
      specs.some((s) => s.type === "mark" && s.cls === "cm-md-codeinfo" && s.from === 3 && s.to === 5)
    ).toBe(true);
    // The closing ``` is hidden too, and its line collapses to a footer strip.
    const closeAt = doc.lastIndexOf("```");
    expect(specsIn(specs, "replace", closeAt, closeAt + 3).length).toBe(1);
    expect(specs.some((s) => s.type === "line" && s.cls === "cm-md-codefoot")).toBe(true);
  });

  it("reveals the raw fences when the cursor is inside the code block", () => {
    const doc = "```js\nlet a=1;\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, 8)); // cursor inside 'let a=1;'
    const closeAt = doc.lastIndexOf("```");
    expect(specsIn(specs, "replace", 0, 3).length).toBe(0); // opening ``` shown
    expect(specsIn(specs, "replace", closeAt, closeAt + 3).length).toBe(0); // closing shown
    expect(specs.some((s) => s.type === "line" && s.cls === "cm-md-codefoot")).toBe(false);
  });

  it("hides the fences of a language-less block without emitting a label", () => {
    const doc = "```\nplain\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    expect(specsIn(specs, "replace", 0, 3).length).toBe(1); // opening ``` hidden
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-codeinfo")).toBe(false);
  });

  it("emits a copy-button spec carrying the fenced block's inner text (fences excluded)", () => {
    const doc = "```js\nlet a=1;\nlet b=2;\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const btn = specs.find((s) => s.type === "copybtn");
    expect(btn).toBeTruthy();
    expect(btn!.from).toBe(0); // anchored to the opening fence line
    expect(btn!.text).toBe("let a=1;\nlet b=2;"); // inner code only
  });

  it("does not emit a copy button for an empty fenced block", () => {
    const doc = "```\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    expect(specs.some((s) => s.type === "copybtn")).toBe(false);
  });

  it("returns specs sorted by position with line decorations first", () => {
    const doc = "# H\n\n**b** text\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    for (let i = 1; i < specs.length; i++) {
      expect(specs[i].from).toBeGreaterThanOrEqual(specs[i - 1].from);
    }
  });

  it("hides the backslash of an escape so `\\*` reads as `*` (off the cursor line)", () => {
    const doc = "x\n\n\\*not bold\n";
    const escAt = doc.indexOf("\\*");
    const specs = computeDecorations(stateFor(doc, 0)); // cursor on line 1, not the escape
    const hid = specsIn(specs, "replace", escAt, escAt + 1);
    expect(hid.length).toBe(1);
    expect(hid[0].from).toBe(escAt);
    expect(hid[0].to).toBe(escAt + 1); // only the backslash, keeping the `*`
  });

  it("reveals the escape when its line holds the cursor", () => {
    const doc = "x\n\n\\*foot\n";
    const escAt = doc.indexOf("\\*");
    const specs = computeDecorations(stateFor(doc, escAt + 1)); // cursor on the escape line
    expect(specsIn(specs, "replace", escAt, escAt + 1).length).toBe(0);
  });

  it("replaces a GFM table block with a single table spec when not editing it", () => {
    const doc = "intro\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\nend\n";
    const specs = computeDecorations(stateFor(doc, 0)); // cursor in "intro"
    const tables = specs.filter((s) => s.type === "table");
    expect(tables.length).toBe(1);
    expect(tables[0].table).toBeTruthy();
    expect(tables[0].table!.header).toEqual(["a", "b"]);
    expect(tables[0].table!.align).toEqual([null, "right"]);
    expect(tables[0].table!.body).toEqual([["1", "2"]]);
    // The table spec spans whole lines, so no inner cell decorations leak out.
    const tFrom = tables[0].from;
    const tTo = tables[0].to;
    expect(specs.some((s) => s.type !== "table" && s.from >= tFrom && s.to <= tTo)).toBe(false);
  });

  // Amended by proposal 0085 Part B (B6): the caret inside a table used to
  // collapse the WHOLE block to raw pipe source. It now reveals only the row
  // being edited — the header run stays a rendered table.
  it("reveals only the active row's source, keeping the rest of the table rendered", () => {
    const doc = "intro\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\nend\n";
    const rowFrom = doc.indexOf("| 1 |");
    const specs = computeDecorations(stateFor(doc, rowFrom + 2));
    const tables = specs.filter((s) => s.type === "table");
    // (a) the active body row is not covered by any table spec …
    expect(tables.some((s) => s.from <= rowFrom && s.to > rowFrom)).toBe(false);
    // (b) … but the header run still renders.
    const headFrom = doc.indexOf("| a |");
    const head = tables.filter((s) => s.from <= headFrom && s.to > headFrom);
    expect(head.length).toBe(1);
    expect(head[0].slice!.header).toBe(true);
    expect(head[0].slice!.whole).toBe(false);
  });
});

// ── Proposal 0085 Part B: row-level reveal ────────────────────────────────────
describe("computeDecorations — table row reveal", () => {
  // A 4-body-row table with a paragraph on either side, so line numbers are
  // never accidentally the same as row indices.
  const doc =
    "intro\n\n| a | b |\n|---|--:|\n| r1 | 1 |\n| r2 | 2 |\n| r3 | 3 |\n| r4 | 4 |\n\nend\n";
  const at = (needle: string) => doc.indexOf(needle);
  const tablesFor = (cursor: number | [number, number]) => {
    const state =
      typeof cursor === "number"
        ? stateFor(doc, cursor)
        : EditorState.create({
            doc,
            selection: EditorSelection.single(cursor[0], cursor[1]),
            extensions: [markdownLanguage],
          });
    return computeDecorations(state).filter((s) => s.type === "table");
  };
  const covers = (specs: DecoSpec[], pos: number) => specs.some((s) => s.from <= pos && s.to > pos);

  it("renders the whole block as one slice when the caret is outside", () => {
    const t = tablesFor(0);
    expect(t.length).toBe(1);
    expect(t[0].from).toBe(at("| a |"));
    expect(t[0].to).toBe(at("| r4 |") + "| r4 | 4 |".length);
    expect(t[0].slice).toEqual({
      header: true,
      bodyFrom: 0,
      bodyTo: 4,
      blockFrom: t[0].from,
      blockTo: t[0].to,
      first: true,
      whole: true,
    });
  });

  it("splits into two slices around the body row under the caret", () => {
    const t = tablesFor(at("| r2 |") + 3);
    expect(t.length).toBe(2);
    expect(covers(t, at("| r2 |"))).toBe(false);
    // header + delimiter + r1
    expect(t[0].slice!.header).toBe(true);
    expect(t[0].slice!.bodyFrom).toBe(0);
    expect(t[0].slice!.bodyTo).toBe(1);
    expect(t[0].slice!.first).toBe(true);
    // r3 + r4, no header, alignment still available from the block's data
    expect(t[1].slice!.header).toBe(false);
    expect(t[1].slice!.bodyFrom).toBe(2);
    expect(t[1].slice!.bodyTo).toBe(4);
    expect(t[1].slice!.first).toBe(false);
    expect(t[1].table!.align).toEqual([null, "right"]);
    // Both slices point at the whole block, which is what the Copy button copies.
    for (const s of t) {
      expect(s.slice!.blockFrom).toBe(at("| a |"));
      expect(s.slice!.blockTo).toBe(at("| r4 |") + "| r4 | 4 |".length);
    }
  });

  it("reveals the header and its delimiter together (B2)", () => {
    const t = tablesFor(at("| a |") + 2);
    expect(covers(t, at("| a |"))).toBe(false);
    expect(covers(t, at("|---|"))).toBe(false);
    expect(t.length).toBe(1);
    expect(t[0].slice!.header).toBe(false);
    expect(t[0].slice!.bodyFrom).toBe(0);
    expect(t[0].slice!.bodyTo).toBe(4);
  });

  it("reveals the header when the caret is on the delimiter line", () => {
    const t = tablesFor(at("|---|") + 2);
    expect(covers(t, at("| a |"))).toBe(false);
    expect(covers(t, at("|---|"))).toBe(false);
    expect(t.length).toBe(1);
    expect(t[0].slice!.header).toBe(false);
  });

  it("reveals every row a multi-line selection touches", () => {
    const t = tablesFor([at("| r1 |") + 2, at("| r2 |") + 2]);
    expect(covers(t, at("| r1 |"))).toBe(false);
    expect(covers(t, at("| r2 |"))).toBe(false);
    expect(t.length).toBe(2);
    expect(t[0].slice!.header).toBe(true);
    expect(t[0].slice!.bodyTo).toBe(0); // header + delimiter only
    expect(t[1].slice!.bodyFrom).toBe(2);
    expect(t[1].slice!.bodyTo).toBe(4);
  });

  it("reveals the header when the selection starts on the line above the table", () => {
    const t = tablesFor([at("intro"), at("| a |") + 2]);
    expect(covers(t, at("| a |"))).toBe(false);
    expect(covers(t, at("|---|"))).toBe(false);
    expect(t.length).toBe(1);
    expect(t[0].slice!.header).toBe(false);
    expect(t[0].slice!.bodyFrom).toBe(0);
    expect(t[0].slice!.bodyTo).toBe(4);
  });

  it("leaves the revealed row's own inline decorations in place", () => {
    const bold = "| a | b |\n|---|---|\n| **x** | 2 |\n";
    const cursor = bold.indexOf("**x**") + 1;
    const specs = computeDecorations(stateFor(bold, cursor));
    expect(specs.some((s) => s.type === "mark" && s.cls === "cm-md-strong")).toBe(true);
    // …and nothing leaks into the rendered header run.
    const head = specs.filter((s) => s.type === "table")[0];
    expect(
      specs.some((s) => s.type !== "table" && s.from >= head.from && s.to <= head.to)
    ).toBe(false);
  });
});

// ── Proposal 0085 Part A: cell-accurate click mapping ─────────────────────────
describe("cellSourceOffset", () => {
  const src = "| a | b |\n|---|--:|\n| 1 | 2 |";

  it("finds the first content character of a header cell", () => {
    expect(cellSourceOffset(src, -1, 0)).toBe(src.indexOf("a"));
    expect(cellSourceOffset(src, -1, 1)).toBe(src.indexOf("b"));
  });

  it("finds the first content character of a body cell", () => {
    expect(cellSourceOffset(src, 0, 0)).toBe(src.indexOf("1"));
    expect(cellSourceOffset(src, 0, 1)).toBe(src.indexOf("2"));
  });

  it("handles rows written without leading/trailing pipes", () => {
    const bare = "a | b\n--- | ---\n1 | 2";
    expect(cellSourceOffset(bare, -1, 0)).toBe(0);
    expect(cellSourceOffset(bare, -1, 1)).toBe(bare.indexOf("b"));
    expect(cellSourceOffset(bare, 0, 0)).toBe(bare.indexOf("1"));
    expect(cellSourceOffset(bare, 0, 1)).toBe(bare.indexOf("2"));
  });

  it("does not treat an escaped pipe as a cell boundary", () => {
    const esc = "| a | b |\n|---|---|\n| x \\| y | 2 |";
    expect(cellSourceOffset(esc, 0, 0)).toBe(esc.indexOf("x"));
    expect(cellSourceOffset(esc, 0, 1)).toBe(esc.lastIndexOf("2"));
    expect(cellSourceOffset(esc, 0, 2)).toBeNull();
  });

  it("returns null for a cell or row that doesn't exist", () => {
    expect(cellSourceOffset(src, -1, 2)).toBeNull();
    expect(cellSourceOffset(src, 5, 0)).toBeNull();
    expect(cellSourceOffset(src, 0, -1)).toBeNull();
  });

  it("parks the caret at the end of an empty cell", () => {
    const empty = "|  | b |\n|---|---|\n| 1 | 2 |";
    expect(cellSourceOffset(empty, -1, 0)).toBe(empty.indexOf("|", 1));
  });
});

// ── Proposal 0085 Part A2: the stale-widget eq() fix ──────────────────────────
describe("TableWidget.eq", () => {
  const src = "| a | b |\n|---|---|\n| 1 | 2 |";
  const slice = {
    header: true,
    bodyFrom: 0,
    bodyTo: 1,
    blockFrom: 0,
    blockTo: src.length,
    first: true,
    whole: true,
  };
  const data = parseTableSource(src)!;

  it("treats identical table text at different offsets as different widgets", () => {
    const a = new TableWidget(data, slice, src, 0);
    const b = new TableWidget(data, { ...slice, blockFrom: 40 }, src, 40);
    expect(a.eq(b)).toBe(false);
    expect(a.eq(new TableWidget(data, slice, src, 0))).toBe(true);
  });

  it("distinguishes two runs of the same block", () => {
    const head = new TableWidget(data, slice, src, 0);
    const body = new TableWidget(
      data,
      { ...slice, header: false, bodyFrom: 0, bodyTo: 1, first: false, whole: false },
      src,
      0
    );
    expect(head.eq(body)).toBe(false);
  });
});

// ── Proposal 0085 Part B4: serializeTable ─────────────────────────────────────
describe("serializeTable", () => {
  it("pads columns and emits the alignment row", () => {
    const t = parseTableSource("| a | bbbb |\n|:-|--:|\n| 1 | 2 |")!;
    expect(serializeTable(t)).toBe("| a   | bbbb |\n| :-- | ---: |\n| 1   | 2    |");
  });

  it("round-trips through parseTableSource", () => {
    for (const src of [
      "| a | b |\n|---|--:|\n| 1 | 2 |",
      "| one | two | three |\n|:-:|---|:--|\n| x | y | z |\n| | | |",
      "| h |\n|---|\n| only |",
      "| a | b |\n|---|---|\n| x \\| y | 2 |",
    ]) {
      const d = parseTableSource(src)!;
      expect(parseTableSource(serializeTable(d))).toEqual(d);
    }
  });

  it("pads a ragged body row instead of throwing", () => {
    const d = { header: ["a", "b", "c"], align: [null, null, null], body: [["1"]] } as const;
    const out = serializeTable(d as unknown as Parameters<typeof serializeTable>[0]);
    const back = parseTableSource(out)!;
    expect(back.header).toEqual(["a", "b", "c"]);
    expect(back.body).toEqual([["1", "", ""]]);
  });

  it("escapes a bare pipe but leaves an already-escaped one alone", () => {
    const out = serializeTable({ header: ["a|b", "c\\|d"], align: [null, null], body: [] });
    expect(out.split("\n")[0]).toBe("| a\\|b | c\\|d |");
  });
});

// ── Proposal 0085 Part B5: Tab / Shift-Tab walk the cells ────────────────────
describe("tableCellTarget", () => {
  const doc = "intro\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\nend\n";
  const step = (cursor: number, forward: boolean) =>
    tableCellTarget(stateFor(doc, cursor), cursor, forward);

  it("returns null outside a table, so the binding falls through", () => {
    expect(step(1, true)).toBeNull();
    expect(step(doc.indexOf("end") + 1, true)).toBeNull();
  });

  it("returns null on the alignment delimiter line", () => {
    expect(step(doc.indexOf("|---|") + 2, true)).toBeNull();
  });

  it("moves to the next cell on the same row", () => {
    expect(step(doc.indexOf("| a |") + 2, true)).toBe(doc.indexOf("b"));
  });

  it("wraps off the last header cell into the first body cell", () => {
    expect(step(doc.indexOf("b"), true)).toBe(doc.indexOf("| 1 |") + 2);
  });

  it("Shift-Tab from the first body cell lands on the last header cell", () => {
    expect(step(doc.indexOf("| 1 |") + 2, false)).toBe(doc.indexOf("b"));
  });

  it("is a consumed no-op at both ends of the table", () => {
    expect(step(doc.indexOf("| 1 | 2 |") + 6, true)).toBe("edge");
    expect(step(doc.indexOf("| a |") + 2, false)).toBe("edge");
  });
});

describe("parseTableSource", () => {
  it("parses header, per-column alignment and body, keeping inline markup", () => {
    const src = "| | kr |\n|---|---:|\n| Cash | 181 188 |\n| **= Total** | **≈ 211 700** |";
    const t = parseTableSource(src)!;
    expect(t.header).toEqual(["", "kr"]);
    expect(t.align).toEqual([null, "right"]);
    expect(t.body).toEqual([
      ["Cash", "181 188"],
      ["**= Total**", "**≈ 211 700**"],
    ]);
  });

  it("returns null for non-tables", () => {
    expect(parseTableSource("just a line")).toBeNull();
  });
});

describe("computeDecorations — task lists", () => {
  it("renders an off-cursor task marker as a checkbox and suppresses its bullet", () => {
    const doc = "- [ ] todo\n- [x] done\n\nz\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1)); // cursor in 'z'
    const boxes = specs.filter((s) => s.type === "checkbox");
    expect(boxes.length).toBe(2);
    // First marker "[ ]" is the 3 chars at 2..5, unchecked.
    expect(boxes[0].from).toBe(2);
    expect(boxes[0].to).toBe(5);
    expect(boxes[0].checked).toBe(false);
    // Second marker "[x]" checked.
    expect(boxes[1].checked).toBe(true);
    // No bullet glyphs for task items (the checkbox is the marker).
    expect(specs.some((s) => s.type === "bullet")).toBe(false);
    // The checked item's line gets the done class.
    expect(specs.some((s) => s.type === "line" && s.cls === "cm-md-task-done")).toBe(true);
  });

  it("reveals the raw marker (no checkbox) when the cursor is on the task line", () => {
    const doc = "- [ ] todo\n- [x] done\n";
    const specs = computeDecorations(stateFor(doc, 4)); // cursor on line 1
    // Line 1's marker is revealed; line 2's still renders a checkbox.
    const boxes = specs.filter((s) => s.type === "checkbox");
    expect(boxes.length).toBe(1);
    expect(boxes[0].from).toBe(doc.indexOf("[x]"));
  });

  it("handles nested and mixed-bullet task items", () => {
    const doc = "- [ ] a\n  * [x] b\n  + [X] c\n\nz\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const boxes = specs.filter((s) => s.type === "checkbox");
    expect(boxes.length).toBe(3);
    expect(boxes.map((b) => b.checked)).toEqual([false, true, true]);
    expect(specs.some((s) => s.type === "bullet")).toBe(false);
  });

  it("still bullets a plain (non-task) list item", () => {
    const doc = "- plain\n- [ ] task\n\nz\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    expect(specs.filter((s) => s.type === "bullet").length).toBe(1); // only the plain one
    expect(specs.filter((s) => s.type === "checkbox").length).toBe(1);
  });
});

describe("linkNodeUrl (clickable links)", () => {
  // resolve the destination URL text for a click at a doc offset.
  function urlAt(doc: string, at: number): string | null {
    return linkNodeUrl(stateFor(doc), at);
  }
  it("returns the destination when clicking the TEXT of a [text](url) link", () => {
    const doc = "see [dibbla](https://dibbla.com) ok";
    const at = doc.indexOf("dibbla]"); // inside the visible link text
    expect(urlAt(doc, at)).toBe("https://dibbla.com");
  });
  it("returns the destination for an image ![alt](url)", () => {
    const doc = "![pic](https://x.com/i.png)";
    expect(urlAt(doc, doc.indexOf("pic"))).toBe("https://x.com/i.png");
  });
  it("returns the URL of a bare autolink", () => {
    const doc = "go https://example.com now";
    expect(urlAt(doc, doc.indexOf("example"))).toBe("https://example.com");
  });
  it("returns the URL of an <angle> autolink", () => {
    const doc = "go <https://example.com> now";
    expect(urlAt(doc, doc.indexOf("example"))).toBe("https://example.com");
  });
  it("returns the address of a bare email", () => {
    const doc = "mail erik@dibbla.com now";
    expect(urlAt(doc, doc.indexOf("erik"))).toBe("erik@dibbla.com");
  });
  it("returns null when the click is not on any link", () => {
    const doc = "just some prose here";
    expect(urlAt(doc, 5)).toBeNull();
  });
});

describe("hrefFromUrlText (autolink normalisation)", () => {
  it("keeps an explicit scheme untouched", () => {
    expect(hrefFromUrlText("https://x.com")).toBe("https://x.com");
    expect(hrefFromUrlText("http://x.com")).toBe("http://x.com");
    expect(hrefFromUrlText("mailto:a@b.com")).toBe("mailto:a@b.com");
    expect(hrefFromUrlText("xmpp:a@b.com")).toBe("xmpp:a@b.com");
  });
  it("prefixes a bare email with mailto:", () => {
    expect(hrefFromUrlText("erik@dibbla.com")).toBe("mailto:erik@dibbla.com");
  });
  it("defaults a schemeless host to https://", () => {
    expect(hrefFromUrlText("www.example.com")).toBe("https://www.example.com");
    expect(hrefFromUrlText("example.com/path")).toBe("https://example.com/path");
  });
  it("trims surrounding whitespace", () => {
    expect(hrefFromUrlText("  https://x.com  ")).toBe("https://x.com");
  });
});

describe("mod-held pointer affordance (compiled CSS + class toggle)", () => {
  // Mount a real EditorView so the base theme's StyleModule is injected, then
  // assert the compiled rule. Regression: a `&`-less selector compiles to a
  // DESCENDANT match (`.ͼx .cm-mod-held …`) which never fires because the class
  // sits on the editor root itself — the selector must compound onto the scope
  // class (`.ͼx.cm-mod-held …`, no space).
  it("compiles the cursor rule against the editor root, not a descendant", async () => {
    const { EditorView } = await import("@codemirror/view");
    const { livePreview } = await import("./livePreview");
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = new EditorView({
      state: EditorState.create({ doc: "x https://a.example\n", extensions: [markdownLanguage, livePreview()] }),
      parent,
    });
    try {
      const css = Array.from(document.querySelectorAll("style"))
        .map((s) => s.textContent ?? "")
        .join("\n");
      // The rule exists…
      expect(css).toContain(".cm-mod-held .cm-md-link");
      // …compounded on the scope class (no space before .cm-mod-held)…
      expect(css).toMatch(/[\w"'\]]\.cm-mod-held \.cm-md-link/);
      // …and never as an unreachable descendant selector.
      expect(css).not.toMatch(/ \.cm-mod-held \.cm-md-link/);
      // The rule must win over CodeMirror's content cursor.
      const rule = css.split("}").find((r) => r.includes(".cm-mod-held .cm-md-link"))!;
      expect(rule).toContain("cursor: pointer !important");

      // And the class toggle reacts to window-level modifier events.
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Meta", metaKey: true }));
      expect(view.dom.classList.contains("cm-mod-held")).toBe(true);
      window.dispatchEvent(new KeyboardEvent("keyup", { key: "Meta", metaKey: false }));
      expect(view.dom.classList.contains("cm-mod-held")).toBe(false);
      // Stuck-class guard: cleared on window blur.
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Control", ctrlKey: true }));
      expect(view.dom.classList.contains("cm-mod-held")).toBe(true);
      window.dispatchEvent(new Event("blur"));
      expect(view.dom.classList.contains("cm-mod-held")).toBe(false);
    } finally {
      view.destroy();
      parent.remove();
    }
  });
});

describe("toggleTaskAt", () => {
  it("flips an unchecked box to checked, touching only one char", () => {
    const src = "- [ ] todo\nother line\n";
    const pos = 0; // anywhere on the task line
    const { next, changed } = toggleTaskAt(src, pos);
    expect(changed).toBe(true);
    expect(next).toBe("- [x] todo\nother line\n");
  });

  it("flips a checked box (any case) back to a space", () => {
    expect(toggleTaskAt("- [x] a\n", 3).next).toBe("- [ ] a\n");
    expect(toggleTaskAt("- [X] a\n", 3).next).toBe("- [ ] a\n");
  });

  it("targets the right item among many, anchored by position", () => {
    const src = "- [ ] a\n- [ ] b\n- [ ] c\n";
    const posB = src.indexOf("] b"); // a position on line 2
    const { next } = toggleTaskAt(src, posB);
    expect(next).toBe("- [ ] a\n- [x] b\n- [ ] c\n");
  });

  it("handles nested / mixed-bullet items and only changes one char", () => {
    const src = "- [ ] a\n  * [ ] b\n  + [ ] c\n";
    const posC = src.indexOf("+");
    const { next, changed } = toggleTaskAt(src, posC);
    expect(changed).toBe(true);
    expect(next).toBe("- [ ] a\n  * [ ] b\n  + [x] c\n");
  });

  it("is a no-op on a non-task line (never a corruption)", () => {
    const src = "just prose with a literal [x] in it\n";
    const r = toggleTaskAt(src, 5);
    expect(r.changed).toBe(false);
    expect(r.next).toBe(src);
  });

  it("is a no-op for an out-of-range position", () => {
    const src = "- [ ] a\n";
    expect(toggleTaskAt(src, -1).changed).toBe(false);
    expect(toggleTaskAt(src, 999).changed).toBe(false);
  });
});

// ── Proposal 0085 Part D3: the pasted table lands as its own block ────────────
describe("tablePasteInsert", () => {
  const md = "| a | b |\n| - | - |";
  const ins = (doc: string, at: number) => tablePasteInsert(stateFor(doc, at), at, at, md);

  it("adds no padding on an empty document", () => {
    expect(ins("", 0)).toBe(md);
  });

  it("breaks out of a paragraph it is pasted into", () => {
    const doc = "hello";
    expect(ins(doc, doc.length)).toBe("\n\n" + md);
  });

  it("adds one newline when the line above is prose", () => {
    const doc = "hello\n";
    expect(ins(doc, doc.length)).toBe("\n" + md);
  });

  it("adds nothing when already surrounded by blank lines", () => {
    const doc = "hello\n\n\n\nend\n";
    expect(ins(doc, doc.indexOf("\n\n") + 2)).toBe(md);
  });

  it("pushes following prose onto its own block", () => {
    const doc = "\nend\n";
    expect(ins(doc, 1)).toBe(md + "\n\n");
  });
});

// ── Proposal 0085 A/B/C, end to end in a real EditorView ─────────────────────
// The pure helpers above are the contract; this block proves CodeMirror accepts
// what they produce — block decorations from the state field, several slices per
// table, and the two widget gestures (click-a-cell, copy-the-table).
describe("the rendered table widget", () => {
  const doc = "intro\n\n| a | b |\n|---|--:|\n| r1 | 1 |\n| r2 | 2 |\n\nend\n";
  // jsdom does no layout, so CodeMirror's measure pass (a requestAnimationFrame
  // after any doc change) trips over the missing Range rect APIs. Stub them to
  // zero rects — nothing here asserts geometry, only structure and behaviour.
  beforeAll(() => {
    const proto = window.Range.prototype as unknown as Record<string, unknown>;
    const empty = { x: 0, y: 0, width: 0, height: 0, top: 0, left: 0, right: 0, bottom: 0 };
    if (typeof proto.getClientRects !== "function") {
      proto.getClientRects = () => Object.assign([], { item: () => null });
      proto.getBoundingClientRect = () => ({ ...empty, toJSON: () => empty });
    }
  });
  const mount = () => {
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = new EditorView({
      state: EditorState.create({
        doc,
        selection: EditorSelection.cursor(0),
        extensions: [markdownLanguage, livePreview()],
      }),
      parent,
    });
    return { view, parent };
  };
  const click = (el: Element) =>
    el.dispatchEvent(new window.MouseEvent("mousedown", { bubbles: true, cancelable: true }));

  it("renders the whole table off the cursor, tagging every cell", () => {
    const { view, parent } = mount();
    expect(parent.querySelectorAll(".cm-md-table").length).toBe(1);
    expect(parent.querySelectorAll(".cm-md-table-copy").length).toBe(1);
    expect(parent.querySelector("th[data-row='-1'][data-col='1']")?.textContent).toBe("b");
    expect(parent.querySelector("td[data-row='1'][data-col='0']")?.textContent).toBe("r2");
    view.destroy();
  });

  it("splits into slices while a row is edited, and heals when the caret leaves", () => {
    const { view, parent } = mount();
    view.dispatch({ selection: { anchor: doc.indexOf("| r1 |") + 3 } });
    expect(parent.querySelectorAll(".cm-md-table").length).toBe(2);
    expect(parent.querySelectorAll(".cm-md-table-copy").length).toBe(1); // first run only
    expect(view.dom.textContent).toContain("| r1 | 1 |"); // the edited row is source

    // Header active → the header + delimiter reveal, the body renders alone.
    view.dispatch({ selection: { anchor: doc.indexOf("| a |") + 2 } });
    expect(parent.querySelectorAll(".cm-md-table").length).toBe(1);
    expect(parent.querySelectorAll("thead").length).toBe(0);

    view.dispatch({ selection: { anchor: 0 } });
    expect(parent.querySelectorAll(".cm-md-table").length).toBe(1);
    expect(parent.querySelectorAll("thead").length).toBe(1);
    view.destroy();
  });

  it("puts the caret in the cell that was clicked, not at the table start", () => {
    const { view, parent } = mount();
    click(parent.querySelector("td[data-row='1'][data-col='1']")!);
    expect(view.state.selection.main.head).toBe(doc.indexOf("| r2 | 2 |") + 7);
    view.dispatch({ selection: { anchor: 0 } });
    click(parent.querySelector("th[data-row='-1'][data-col='0']")!);
    expect(view.state.selection.main.head).toBe(doc.indexOf("| a |") + 2);
    view.destroy();
  });

  // Part A2: identical table text at a new offset must be a NEW widget. With
  // the old `eq` (src only) CodeMirror reused the DOM and its closure over the
  // stale `from`, so every per-cell offset was computed against the wrong base.
  it("still lands in the right cell after text is inserted above the table", () => {
    const { view, parent } = mount();
    view.dispatch({ changes: { from: 0, insert: "preamble\n\n" }, selection: { anchor: 0 } });
    const shifted = view.state.doc.toString();
    click(parent.querySelector("td[data-row='1'][data-col='1']")!);
    expect(view.state.selection.main.head).toBe(shifted.indexOf("| r2 | 2 |") + 7);
    view.destroy();
  });

  // Part D: a spreadsheet paste becomes a GFM table in ONE transaction, so a
  // single undo restores the document as if it never happened.
  it("converts a TSV paste into an aligned table, and leaves other pastes alone", () => {
    const { view } = mount();
    const paste = (flavours: Record<string, string>) => {
      const ev = new window.Event("paste", { bubbles: true, cancelable: true });
      Object.defineProperty(ev, "clipboardData", {
        value: { getData: (t: string) => flavours[t] ?? "" },
      });
      view.contentDOM.dispatchEvent(ev);
      return ev.defaultPrevented;
    };
    view.dispatch({ selection: { anchor: view.state.doc.length } });
    expect(paste({ "text/plain": "x\ty\n1\t2" })).toBe(true);
    expect(view.state.doc.toString()).toContain("| x   | y   |\n| --- | --- |\n| 1   | 2   |");
    // A non-table paste never reaches the converter: the text lands verbatim.
    paste({ "text/plain": "just some prose" });
    expect(view.state.doc.toString()).toContain("just some prose");
    expect(view.state.doc.toString()).not.toContain("| just some prose |");
    view.destroy();
  });

  it("copies the table's own bytes, verbatim, without moving the caret", async () => {
    const written: string[] = [];
    const nav = navigator as unknown as { clipboard?: unknown };
    const prev = nav.clipboard;
    const prevSecure = window.isSecureContext;
    Object.defineProperty(window, "isSecureContext", { configurable: true, value: true });
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: (t: string) => {
          written.push(t);
          return Promise.resolve();
        },
      },
    });
    try {
      const { view, parent } = mount();
      const btn = parent.querySelector(".cm-md-table-copy")!;
      const notCancelled = click(btn);
      await Promise.resolve();
      expect(written).toEqual(["| a | b |\n|---|--:|\n| r1 | 1 |\n| r2 | 2 |"]);
      expect(notCancelled).toBe(false); // preventDefault: no blur, no caret move
      expect(view.state.selection.main.head).toBe(0);
      view.destroy();
    } finally {
      Object.defineProperty(window, "isSecureContext", { configurable: true, value: prevSecure });
      if (prev === undefined) delete (navigator as unknown as { clipboard?: unknown }).clipboard;
      else Object.defineProperty(navigator, "clipboard", { configurable: true, value: prev });
    }
  });
});
