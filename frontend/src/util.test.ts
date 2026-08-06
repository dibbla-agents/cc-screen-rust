import { describe, expect, it } from "vitest";
import { buildSharedMap, machineAccent, sharedEntry, sharedOwner, sharedVia } from "./util";
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
