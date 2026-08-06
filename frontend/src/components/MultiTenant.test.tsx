// Proposal 0056 — the LimitCard (Part B) and the post-activation hand-off
// (Part A1). The LimitCard assertions are static renders; the ActivatePage
// flows drive the real component against a mocked api module (the "mocked 402"
// approve path and the success path offering "Start your first session").

import { renderToStaticMarkup } from "react-dom/server";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ActivatePage, LimitCard } from "./MultiTenant";
import type { MePlan } from "../api";

vi.mock("../api", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../api")>();
  return {
    ...mod,
    approveDevice: vi.fn(),
    listAgents: vi.fn().mockResolvedValue([]),
  };
});
import { approveDevice, listAgents } from "../api";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const plan: MePlan = { name: "free", maxAgents: 10, maxSessions: 50, agents: 10 };

describe("LimitCard (proposal 0056 Part B)", () => {
  it("renders the machine-cap card with plan name, cap, and the upgrade mailto", () => {
    const html = renderToStaticMarkup(<LimitCard plan={plan} what="machines" support="erik@dibbla.com" />);
    expect(html).toContain("Plan limit reached");
    expect(html).toContain("free");
    expect(html).toContain("10");
    expect(html).toContain("machines");
    expect(html).toContain("Unlink a machine");
    expect(html).toContain("mailto:erik@dibbla.com?subject=cc-screen%20plan%20upgrade%20(machines)");
    expect(html).toContain("Request an upgrade");
    expect(html).toContain("free during the beta");
  });

  it("renders the session-cap card and stays useful with no plan / no support", () => {
    const html = renderToStaticMarkup(<LimitCard plan={plan} what="sessions" support="s@x.com" />);
    expect(html).toContain("concurrent sessions");
    expect(html).toContain("50");
    expect(html).not.toContain("Unlink a machine");

    // No plan / no support (single-tenant or a stale /api/me): generic copy,
    // no dead mailto button.
    const bare = renderToStaticMarkup(<LimitCard what="sessions" />);
    expect(bare).toContain("a limited number of");
    expect(bare).not.toContain("mailto:");
  });
});

// ── Interactive ActivatePage flows against the mocked api ─────────────────────

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("ActivatePage (proposal 0056 A1/B2)", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.clearAllMocks();
  });

  async function submitCode(el: React.ReactElement) {
    await act(async () => root.render(el));
    const input = container.querySelector("input")!;
    await act(async () => setInputValue(input, "WDJB-MJHT"));
    const form = container.querySelector("form")!;
    await act(async () => {
      form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
  }

  it("offers 'Start your first session' on the success card when wired", async () => {
    vi.mocked(approveDevice).mockResolvedValue({ ok: true, machine: "pine" });
    vi.mocked(listAgents).mockResolvedValue([
      { agentId: "a1", machine: "pine", online: true, createdAt: 0, missing: [] },
    ]);
    const onStart = vi.fn();
    await submitCode(<ActivatePage email="e@x.com" onDone={() => {}} onStartSession={onStart} />);

    expect(container.innerHTML).toContain("connected");
    const buttons = Array.from(container.querySelectorAll("button"));
    const start = buttons.find((b) => b.textContent === "Start your first session");
    expect(start, "success card offers the hand-off").toBeTruthy();
    await act(async () => start!.click());
    expect(onStart).toHaveBeenCalledWith("pine");
  });

  it("renders the LimitCard (not an error line) from a mocked 402 approve", async () => {
    vi.mocked(approveDevice).mockResolvedValue({
      ok: false,
      limit: true,
      error: "Machine limit reached for your plan (10). Unlink one or ask for an upgrade.",
    });
    await submitCode(
      <ActivatePage email="e@x.com" plan={plan} support="erik@dibbla.com" onDone={() => {}} />
    );

    expect(container.innerHTML).toContain("Plan limit reached");
    expect(container.innerHTML).toContain("mailto:erik@dibbla.com");
    // The raw error line is replaced by the card.
    expect(container.innerHTML).not.toContain("Machine limit reached for your plan");
  });

  it("keeps the plain error line for a non-limit failure", async () => {
    vi.mocked(approveDevice).mockResolvedValue({ ok: false, error: "Unknown or expired code" });
    await submitCode(<ActivatePage email="e@x.com" plan={plan} onDone={() => {}} />);
    expect(container.innerHTML).toContain("Unknown or expired code");
    expect(container.innerHTML).not.toContain("Plan limit reached");
  });

  // Proposal 0060 B6: a terminal sign-in (kind 'client') gets its own success
  // copy — no machine language, no install/start-session hand-off, no agent
  // polling (nothing registered).
  it("says 'Terminal signed in' with the label for a client-kind approve", async () => {
    vi.mocked(approveDevice).mockResolvedValue({
      ok: true,
      machine: "erik@orchid",
      kind: "client",
      label: "erik@orchid",
    });
    const onStart = vi.fn();
    await submitCode(<ActivatePage email="e@x.com" onDone={() => {}} onStartSession={onStart} />);

    expect(container.innerHTML).toContain("Terminal signed in");
    expect(container.innerHTML).toContain("erik@orchid");
    expect(container.innerHTML).not.toContain("connected to your machines");
    const buttons = Array.from(container.querySelectorAll("button"));
    expect(
      buttons.find((b) => b.textContent === "Start your first session"),
      "no machine hand-off on a terminal sign-in"
    ).toBeFalsy();
    expect(listAgents, "no agent polling for a client sign-in").not.toHaveBeenCalled();
  });

  it("treats a pre-0060 approve response (no kind) as the machine flow", async () => {
    // api.ts defaults kind to 'agent' when the hub omits it — the old success
    // card, byte-for-byte.
    vi.mocked(approveDevice).mockResolvedValue({ ok: true, machine: "pine", kind: "agent", label: "pine" });
    vi.mocked(listAgents).mockResolvedValue([
      { agentId: "a1", machine: "pine", online: true, createdAt: 0, missing: [] },
    ]);
    await submitCode(<ActivatePage email="e@x.com" onDone={() => {}} />);
    expect(container.innerHTML).toContain("connected");
    expect(container.innerHTML).not.toContain("Terminal signed in");
  });
});
