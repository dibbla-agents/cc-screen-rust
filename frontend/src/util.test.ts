import { describe, expect, it } from "vitest";
import { buildSharedMap, fuzzyScore, machineAccent, sameJson, sharedEntry, sharedOwner, sharedVia, statusDot } from "./util";
import type { ReceivedShare } from "./api";

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

// The shared-vs-owned lookup (proposal 0041) is driven by the received-shares
// feed: an agent grant marks the whole box; a session grant marks one
// (machine,name) and wins over the box-wide grant.
describe("sharedOwner / buildSharedMap", () => {
  const share = (p: Partial<ReceivedShare>): ReceivedShare => ({
    id: "x",
    agentId: "a",
    machine: "laptop",
    kind: "agent",
    permission: "use",
    ownerEmail: "sam@example.com",
    createdAt: 0,
    ...p,
  });

  it("returns null for an unshared session / no map", () => {
    expect(sharedOwner(null, "laptop", "claude-x")).toBeNull();
    const map = buildSharedMap([]);
    expect(sharedOwner(map, "laptop", "claude-x")).toBeNull();
  });

  it("marks the whole machine for an agent grant", () => {
    const map = buildSharedMap([share({ kind: "agent" })]);
    expect(sharedOwner(map, "laptop")).toBe("sam@example.com");
    expect(sharedOwner(map, "laptop", "any-session")).toBe("sam@example.com");
    expect(sharedOwner(map, "other-box", "s")).toBeNull();
  });

  it("marks only the named session for a session grant", () => {
    const map = buildSharedMap([
      share({ kind: "session", session: "claude-x", permission: "view", ownerEmail: "ana@x.com" }),
    ]);
    expect(sharedOwner(map, "laptop", "claude-x")).toBe("ana@x.com");
    expect(sharedOwner(map, "laptop", "claude-y")).toBeNull();
    expect(sharedOwner(map, "laptop")).toBeNull();
  });

  it("prefers the more specific session grant over a machine-wide one", () => {
    const map = buildSharedMap([
      share({ kind: "agent", ownerEmail: "box@x.com" }),
      share({ kind: "session", session: "claude-x", ownerEmail: "sess@x.com" }),
    ]);
    expect(sharedOwner(map, "laptop", "claude-x")).toBe("sess@x.com");
    expect(sharedOwner(map, "laptop", "claude-y")).toBe("box@x.com");
  });
});

// Proposal 0062 — shared ranking test-vectors with the Rust port in the ccs
// TUI (`crates/tui/src/app.rs`, test `fuzzy_score_web_matches_the_web_scorer`).
// Same inputs, same exact expected values on both sides, so the two scorers
// can't drift silently. If you change one scorer, change the other AND both
// vector sets together.
//
// The tier-ordering half of the contract (NAME_TIER=100_000 > PATH_TIER=10_000
// > META_TIER=0, Rust test `score_session_tiers_name_over_path_over_meta`) has
// no web-side seam: `scoreItem` and the tier constants are non-exported
// internals of the SessionDrawer component (a useCallback), and the existing
// SessionDrawer tests are static-markup renders that can't type a query — so
// only the fuzzyScore vectors are asserted here.
describe("fuzzyScore — 0062 parity vectors with the ccs TUI scorer", () => {
  it("no match / empty inputs", () => {
    expect(fuzzyScore("xyz", "docs")).toBeNull();
    expect(fuzzyScore("", "anything")).toBe(0);
    expect(fuzzyScore("a", "")).toBeNull();
  });

  it("bonus arithmetic — exact values", () => {
    // Head-of-string hit: +2 (char) +6 (head) +12 (word start) = 20.
    expect(fuzzyScore("d", "dev")).toBe(20);
    // Word-start hit after a separator: +2 +12 = 14.
    expect(fuzzyScore("d", "my-dev")).toBe(14);
    // Mid-word hit: just +2.
    expect(fuzzyScore("e", "dev")).toBe(2);
    // Contiguous run: "de" on "dev" = 20 + (2 + 4·1) = 26.
    expect(fuzzyScore("de", "dev")).toBe(26);
  });

  it("case-insensitive", () => {
    expect(fuzzyScore("DE", "dev")).toBe(fuzzyScore("de", "dev"));
  });

  it("a contiguous prefix run beats a scattered mid-word subsequence", () => {
    expect(fuzzyScore("dev", "development")!).toBeGreaterThan(
      fuzzyScore("dev", "widevine")!
    );
  });

  it("word-start bonuses can make up for a broken run (deliberate tie)", () => {
    // "dev" ties on "development" (d@head + contiguous e,v) and
    // "daemon-vault" (d@head, scattered e, v@word-start).
    expect(fuzzyScore("dev", "development")).toBe(36);
    expect(fuzzyScore("dev", "daemon-vault")).toBe(36);
  });
});

// Team-origin grants (proposal 0065 Part B): a materialized team row marks the
// whole box like an agent grant, but reports "team" via sharedVia and carries
// the org name for the chip tooltip. A direct grant always wins the owner
// string; team is reported only when that's all there is.
describe("sharedVia / team grants", () => {
  const share = (p: Partial<ReceivedShare>): ReceivedShare => ({
    id: "x",
    agentId: "a",
    machine: "laptop",
    kind: "agent",
    permission: "use",
    ownerEmail: "sam@example.com",
    createdAt: 0,
    ...p,
  });
  const teamRow = (p: Partial<ReceivedShare> = {}): ReceivedShare =>
    share({ kind: "team", origin: "team", permission: "view", orgName: "acme-eng", ...p });

  it("returns null for no map / an unshared machine", () => {
    expect(sharedVia(null, "laptop", "s")).toBeNull();
    expect(sharedVia(buildSharedMap([]), "laptop")).toBeNull();
  });

  it("carries the team flag + org name through buildSharedMap", () => {
    const map = buildSharedMap([teamRow()]);
    const e = sharedEntry(map, "laptop");
    expect(e).toEqual({ owner: "sam@example.com", team: true, orgName: "acme-eng" });
    expect(sharedVia(map, "laptop")).toBe("team");
    // Any session on the box inherits the machine-wide team visibility.
    expect(sharedVia(map, "laptop", "claude-x")).toBe("team");
    expect(sharedOwner(map, "laptop", "claude-x")).toBe("sam@example.com");
  });

  it("reports 'direct' for an explicit share (origin absent = old hub)", () => {
    const map = buildSharedMap([share({ kind: "agent" })]);
    expect(sharedVia(map, "laptop")).toBe("direct");
    const withOrigin = buildSharedMap([share({ kind: "agent", origin: "direct" })]);
    expect(sharedVia(withOrigin, "laptop")).toBe("direct");
  });

  it("lets a direct grant win over a team grant, either insertion order", () => {
    for (const rows of [
      [share({ kind: "agent", ownerEmail: "direct@x.com" }), teamRow({ ownerEmail: "team@x.com" })],
      [teamRow({ ownerEmail: "team@x.com" }), share({ kind: "agent", ownerEmail: "direct@x.com" })],
    ]) {
      const map = buildSharedMap(rows);
      expect(sharedVia(map, "laptop")).toBe("direct");
      expect(sharedOwner(map, "laptop")).toBe("direct@x.com");
    }
  });

  it("keeps a direct session grant 'direct' on a team-visible machine", () => {
    const map = buildSharedMap([
      teamRow({ ownerEmail: "team@x.com" }),
      share({ kind: "session", session: "claude-x", permission: "view", ownerEmail: "sess@x.com" }),
    ]);
    expect(sharedVia(map, "laptop", "claude-x")).toBe("direct");
    expect(sharedOwner(map, "laptop", "claude-x")).toBe("sess@x.com");
    // The rest of the box stays team-badged.
    expect(sharedVia(map, "laptop", "claude-y")).toBe("team");
  });
});

// Proposal 0068 — the poll-equality helper and the now-static status dot.
describe("sameJson + statusDot (proposal 0068)", () => {
  it("treats a re-serialized identical payload as unchanged", () => {
    const a = [{ name: "x", waiting: true }];
    const b = JSON.parse(JSON.stringify(a));
    expect(sameJson(a, b)).toBe(true);
    expect(sameJson(a, [{ name: "x", waiting: false }])).toBe(false);
  });

  it("carries no infinite animation class — colour alone encodes the state", () => {
    for (const s of ["running", "ready", "error"] as const) {
      expect(statusDot(s)).not.toContain("animate-");
    }
    // The three states still read differently.
    expect(new Set(["running", "ready", "error"].map((s) => statusDot(s as never))).size).toBe(3);
  });
});
