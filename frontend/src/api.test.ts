import { afterEach, describe, expect, it, vi } from "vitest";
import {
  downloadURL,
  fetchMachines,
  inlineURL,
  watchURL,
  wsURL,
} from "./api";

// The URL builders read `location` for scheme/host. jsdom gives us a default
// http://localhost; we stub it so the wss/host derivation is deterministic and
// independent of the test runner's origin.
function stubLocation(protocol: string, host: string) {
  vi.stubGlobal("location", { protocol, host } as Location);
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("wsURL", () => {
  it("derives wss from an https page and omits machine when absent", () => {
    stubLocation("https:", "hub.example:8840");
    expect(wsURL("claude-x")).toBe(
      "wss://hub.example:8840/api/ws?session=claude-x"
    );
  });

  it("derives ws from an http page", () => {
    stubLocation("http:", "10.0.0.2:8839");
    expect(wsURL("claude-x")).toBe("ws://10.0.0.2:8839/api/ws?session=claude-x");
  });

  it("appends &machine= (encoded) when a machine is given", () => {
    stubLocation("https:", "hub.example");
    expect(wsURL("claude-x", "box A")).toBe(
      "wss://hub.example/api/ws?session=claude-x&machine=box%20A"
    );
  });

  it("treats an empty machine as no machine (single-agent / no hub)", () => {
    stubLocation("https:", "hub.example");
    expect(wsURL("claude-x", "")).toBe(
      "wss://hub.example/api/ws?session=claude-x"
    );
  });
});

describe("watchURL", () => {
  it("omits machine when absent and appends it when present", () => {
    stubLocation("https:", "hub.example");
    expect(watchURL()).toBe("wss://hub.example/api/watch");
    expect(watchURL("laptop")).toBe("wss://hub.example/api/watch?machine=laptop");
  });
});

describe("download/inline URL builders", () => {
  it("download omits/appends machine with the right separator", () => {
    expect(downloadURL("a/b.png")).toBe("/api/download?path=a%2Fb.png");
    expect(downloadURL("a/b.png", "laptop")).toBe(
      "/api/download?path=a%2Fb.png&machine=laptop"
    );
  });

  it("inline (pdf.js) keeps inline=1 and appends machine", () => {
    expect(inlineURL("a.pdf")).toBe("/api/download?inline=1&path=a.pdf");
    expect(inlineURL("a.pdf", "laptop")).toBe(
      "/api/download?inline=1&path=a.pdf&machine=laptop"
    );
  });
});

describe("fetchMachines", () => {
  it("returns the roster on success", async () => {
    const roster = [{ machine: "a", hostname: "alpha", online: true }];
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: true, json: async () => roster })
    );
    await expect(fetchMachines()).resolves.toEqual(roster);
  });

  it("returns [] on a 404 (standalone agent has no /api/machines)", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 404 }));
    await expect(fetchMachines()).resolves.toEqual([]);
  });

  it("returns [] when fetch rejects (network error)", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("boom")));
    await expect(fetchMachines()).resolves.toEqual([]);
  });
});
