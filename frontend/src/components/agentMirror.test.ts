import { describe, it, expect } from "vitest";
import { fitFontSize, readableCols, MIN_AGENT_COLS, type CellRatios } from "./AgentMirror";

// Typical monospace cell metrics (per 1px of font size).
const R: CellRatios = { w: 0.6, h: 1.2 };

describe("fitFontSize", () => {
  it("shrinks the font so a wide grid fits a narrow column (width-bound)", () => {
    // 120 cols in 400px is the headline case: keep the grid, drop the font.
    const f = fitFontSize(120, 30, 400, 2000, R, 16);
    expect(f).toBe(5); // floor(400*0.997 / (120*0.6)) = floor(5.53)
    // And the rendered width never exceeds the box (worst case: a clip).
    expect(f * 120 * R.w).toBeLessThanOrEqual(400);
  });

  it("caps at maxFontSize when there is plenty of room", () => {
    expect(fitFontSize(80, 24, 2000, 2000, R, 14)).toBe(14);
  });

  it("is height-bound in a short, wide column", () => {
    // 50 rows in 300px tall forces the height constraint to win.
    const f = fitFontSize(40, 50, 2000, 300, R, 16);
    expect(f).toBe(5); // floor(300 / (50*1.2)) = 5
    expect(f * 50 * R.h).toBeLessThanOrEqual(300);
  });

  it("never returns below the 4px floor", () => {
    expect(fitFontSize(400, 200, 100, 100, R, 16)).toBe(4);
  });

  it("falls back gracefully on a degenerate (unmeasured) grid", () => {
    expect(fitFontSize(0, 0, 400, 400, R, 16)).toBe(12);
    expect(fitFontSize(80, 24, 0, 0, R, 16)).toBe(12);
  });
});

describe("readableCols", () => {
  it("narrows the report to what fits at the target font (Option A)", () => {
    // 420px column, 120-col grid, 14px target → ~49 cols (not 120).
    expect(readableCols(420, 120, R, 14)).toBe(49); // floor(420*0.997 / (14*0.6))
  });

  it("makes the agent legible vs. the old keep-120-and-shrink-font path", () => {
    const cols = readableCols(420, 120, R, 14);
    const fontNarrow = fitFontSize(cols, 30, 420, 800, R, 14);
    const fontOld = fitFontSize(120, 30, 420, 800, R, 14); // never reported, font-only
    expect(fontNarrow).toBeGreaterThanOrEqual(11); // readable
    expect(fontOld).toBeLessThan(7); // the tiny-font problem this fixes
  });

  it("floors at MIN_AGENT_COLS in a very thin column", () => {
    // 280px is the column's min width; don't strangle the agent below 40 cols.
    expect(readableCols(280, 120, R, 14)).toBe(MIN_AGENT_COLS); // fit≈33 → 40
  });

  it("never widens past the grid's own width", () => {
    expect(readableCols(1200, 120, R, 14)).toBe(120); // fit≈142 → capped at 120
    expect(readableCols(420, 30, R, 14)).toBe(30); // grid already narrower than fit
  });

  it("falls back to the grid width on a degenerate column", () => {
    expect(readableCols(0, 120, R, 14)).toBe(120);
    expect(readableCols(420, 0, R, 14)).toBe(49); // gridCols 0 → defaults to 80, fit 49
  });
});
