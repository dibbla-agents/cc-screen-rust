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

// ── Proposal 0056 Part B — the 402 plan-limit plumbing ─────────────────────────

describe("approveDevice (proposal 0056 B2)", () => {
  it("no longer swallows the body: a 402 sets limit + the server's message", async () => {
    const { approveDevice } = await import("./api");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 402,
        text: async () => "Machine limit reached for your plan (10). Unlink one or ask for an upgrade.",
      })
    );
    await expect(approveDevice("WDJB-MJHT")).resolves.toEqual({
      ok: false,
      limit: true,
      error: "Machine limit reached for your plan (10). Unlink one or ask for an upgrade.",
    });
  });

  it("keeps the friendly 404 and flags a non-402 failure as limit:false", async () => {
    const { approveDevice } = await import("./api");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: false, status: 404, text: async () => "unknown or expired code" })
    );
    await expect(approveDevice("WDJB-MJHT")).resolves.toEqual({
      ok: false,
      error: "Unknown or expired code",
    });

    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: false, status: 500, text: async () => "boom" })
    );
    await expect(approveDevice("WDJB-MJHT")).resolves.toEqual({
      ok: false,
      limit: false,
      error: "boom",
    });
  });
});

describe("createSession errors (proposal 0056 B2)", () => {
  it("throws an ApiError carrying the 402 status + the server's message", async () => {
    const { createSession, ApiError } = await import("./api");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: false,
        status: 402,
        text: async () => "Session limit reached for your plan (50).",
      })
    );
    const err = await createSession("cc", "x", "/tmp").catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as InstanceType<typeof ApiError>).status).toBe(402);
    expect((err as Error).message).toBe("Session limit reached for your plan (50).");
  });
});

describe("createShare (proposal 0056 C2)", () => {
  it("returns the unified shape with the invite link mapped to inviteUrl", async () => {
    const { createShare } = await import("./api");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ id: "i1", status: "pending", invite_url: "/invite/tok123" }),
      })
    );
    await expect(
      createShare({ granteeEmail: "ghost@x.com", machine: "laptop" })
    ).resolves.toEqual({ id: "i1", status: "pending", inviteUrl: "/invite/tok123" });
  });
});
