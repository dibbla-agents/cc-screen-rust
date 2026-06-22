import { describe, it, expect } from "vitest";
import { codeHighlight } from "./codeHighlight";

// The highlight style compiles its tag→style specs into a StyleModule of CSS
// rules. We assert the generated CSS references our --cc-syn-* tokens verbatim
// (rather than snapshotting their values) — that is what makes the palette
// single-sourced in index.css and a future light mode a token swap. It also
// guards the Part C caveat: if a CodeMirror build ever inlined the var() value,
// these assertions would fail loudly.
const rules = codeHighlight.module?.getRules() ?? "";

describe("codeHighlight", () => {
  it("compiles to a non-empty StyleModule", () => {
    expect(rules.length).toBeGreaterThan(0);
  });

  it("maps the high-value tags to their --cc-syn-* tokens (kept as live var())", () => {
    for (const token of [
      "--cc-syn-keyword",
      "--cc-syn-string",
      "--cc-syn-number",
      "--cc-syn-comment",
      "--cc-syn-function",
      "--cc-syn-type",
      "--cc-syn-variable",
      "--cc-syn-punct",
    ]) {
      expect(rules).toContain(`var(${token})`);
    }
  });

  it("renders comments in italic", () => {
    expect(rules).toContain("font-style: italic");
  });
});
