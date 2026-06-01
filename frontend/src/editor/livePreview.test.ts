import { describe, it, expect } from "vitest";
import { EditorState, EditorSelection } from "@codemirror/state";
import { markdownLanguage } from "@codemirror/lang-markdown";
import { computeDecorations, parseTableSource, type DecoSpec } from "./livePreview";

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

  it("backgrounds fenced code lines without hiding the fences", () => {
    const doc = "```js\nlet a=1;\n```\n\nq\n";
    const specs = computeDecorations(stateFor(doc, doc.length - 1));
    const cb = specs.filter((s) => s.type === "line" && s.cls === "cm-md-codeblock");
    expect(cb.length).toBeGreaterThanOrEqual(2); // at least the fence + code line
    // The fence backticks are NOT hidden (we keep code blocks literal).
    expect(specsIn(specs, "replace", 0, 3).length).toBe(0);
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

  it("reveals table source (no table widget) when the cursor is inside it", () => {
    const doc = "intro\n\n| a | b |\n|---|--:|\n| 1 | 2 |\n\nend\n";
    const cursor = doc.indexOf("| 1 |") + 2;
    const specs = computeDecorations(stateFor(doc, cursor));
    expect(specs.filter((s) => s.type === "table").length).toBe(0);
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
