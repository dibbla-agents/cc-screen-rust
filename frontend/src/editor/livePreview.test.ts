import { describe, it, expect } from "vitest";
import { EditorState, EditorSelection } from "@codemirror/state";
import { markdownLanguage } from "@codemirror/lang-markdown";
import { computeDecorations, parseTableSource, toggleTaskAt, type DecoSpec } from "./livePreview";

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
