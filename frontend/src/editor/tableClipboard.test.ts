import { describe, it, expect } from "vitest";
import { htmlTableToMarkdown, tsvToTable } from "./tableClipboard";
import { serializeTable } from "./livePreview";

// Proposal 0085 Part D. Both converters are the *detection* as well as the
// parse: returning null is how a paste falls through to CodeMirror's default,
// which is the behaviour every non-table paste must keep.

describe("tsvToTable", () => {
  it("converts a 2×2 block", () => {
    const t = tsvToTable("a\tb\n1\t2")!;
    expect(t.header).toEqual(["a", "b"]);
    expect(t.body).toEqual([["1", "2"]]);
    expect(t.align).toEqual([null, null]);
    expect(serializeTable(t)).toBe("| a   | b   |\n| --- | --- |\n| 1   | 2   |");
  });

  it("ignores a trailing empty line", () => {
    expect(tsvToTable("a\tb\n1\t2\n")!.body).toEqual([["1", "2"]]);
  });

  it("handles CRLF input", () => {
    const t = tsvToTable("a\tb\r\n1\t2\r\n")!;
    expect(t.header).toEqual(["a", "b"]);
    expect(t.body).toEqual([["1", "2"]]);
  });

  it("keeps a ragged block (serializeTable pads it)", () => {
    const t = tsvToTable("a\tb\tc\n1\t2")!;
    expect(t.header).toEqual(["a", "b", "c"]);
    expect(t.body).toEqual([["1", "2"]]);
    expect(serializeTable(t).split("\n")[2]).toBe("| 1   | 2   |     |");
  });

  it("refuses text with tabs on only some lines", () => {
    expect(tsvToTable("a\tb\nplain prose\n1\t2")).toBeNull();
  });

  it("refuses a single line and a single cell", () => {
    expect(tsvToTable("a\tb")).toBeNull();
    expect(tsvToTable("just one cell\n")).toBeNull();
  });
});

describe("htmlTableToMarkdown", () => {
  it("converts an Excel-flavour <thead>-less table", () => {
    const t = htmlTableToMarkdown(
      "<meta charset=utf-8><table><tr><td>a</td><td>b</td></tr><tr><td>1</td><td>2</td></tr></table>"
    )!;
    expect(t.header).toEqual(["a", "b"]);
    expect(t.body).toEqual([["1", "2"]]);
    expect(t.align).toEqual([null, null]);
  });

  it("uses a <thead> row as the header", () => {
    const t = htmlTableToMarkdown(
      "<table><thead><tr><th>H1</th><th>H2</th></tr></thead><tbody><tr><td>x</td><td>y</td></tr></tbody></table>"
    )!;
    expect(t.header).toEqual(["H1", "H2"]);
    expect(t.body).toEqual([["x", "y"]]);
  });

  it("refuses html with two tables", () => {
    expect(
      htmlTableToMarkdown(
        "<table><tr><td>a</td></tr><tr><td>b</td></tr></table><table><tr><td>c</td></tr><tr><td>d</td></tr></table>"
      )
    ).toBeNull();
  });

  it("refuses html with meaningful text outside the table", () => {
    expect(
      htmlTableToMarkdown(
        "<p>read this first</p><table><tr><td>a</td></tr><tr><td>b</td></tr></table>"
      )
    ).toBeNull();
  });

  it("tolerates the <style>/<meta> wrapper clipboard apps emit", () => {
    const t = htmlTableToMarkdown(
      "<meta charset=utf-8><style>td{color:red}</style><table><tr><td>a</td><td>b</td></tr><tr><td>1</td><td>2</td></tr></table>"
    )!;
    expect(t.header).toEqual(["a", "b"]);
  });

  it("takes textContent only — markup is never interpreted or re-emitted", () => {
    const t = htmlTableToMarkdown(
      '<table><tr><td>a|b</td><td><b>bold</b> <img src=x onerror=alert(1)></td></tr>' +
        "<tr><td>1</td><td>2</td></tr></table>"
    )!;
    expect(t.header).toEqual(["a|b", "bold"]);
    // The pipe is escaped on the way into the document; no markup survives.
    const md = serializeTable(t);
    expect(md.split("\n")[0]).toBe("| a\\|b | bold |");
    expect(md).not.toContain("<");
  });

  it("flattens a cell's internal newlines to single spaces", () => {
    const t = htmlTableToMarkdown(
      "<table><tr><td>one\n  two</td><td>b</td></tr><tr><td>1</td><td>2</td></tr></table>"
    )!;
    expect(t.header).toEqual(["one two", "b"]);
  });

  it("refuses a single-row (single-cell) copy", () => {
    expect(htmlTableToMarkdown("<table><tr><td>only</td></tr></table>")).toBeNull();
  });

  it("refuses html with no table at all", () => {
    expect(htmlTableToMarkdown("<p>hello</p>")).toBeNull();
  });
});
