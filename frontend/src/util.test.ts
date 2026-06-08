import { describe, expect, it } from "vitest";
import { machineAccent } from "./util";

// machineAccent backs the per-pane identity bar (proposal 0021). The contract:
// deterministic per machine id, null for the empty machine, and same-machine
// panes share a colour while different machines (usually) differ.
describe("machineAccent", () => {
  it("returns null for the empty machine (single-agent / no hub)", () => {
    expect(machineAccent("")).toBeNull();
  });

  it("is deterministic — same id maps to the same colour every call", () => {
    const a = machineAccent("pine");
    const b = machineAccent("pine");
    expect(a).not.toBeNull();
    expect(a).toEqual(b);
  });

  it("gives different machines different hues", () => {
    const pine = machineAccent("pine");
    const studio = machineAccent("studio");
    expect(pine!.spine).not.toBe(studio!.spine);
  });

  it("emits valid hsl() triplets with fixed S/L", () => {
    const acc = machineAccent("mac-studio-ubuntu")!;
    expect(acc.spine).toMatch(/^hsl\(\d{1,3} 62% 55%\)$/);
    expect(acc.text).toMatch(/^hsl\(\d{1,3} 70% 74%\)$/);
    expect(acc.tint).toMatch(/^hsl\(\d{1,3} 55% 50% \/ 0\.12\)$/);
  });
});
