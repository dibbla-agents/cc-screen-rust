// Proposal 0056 C2 — the share form's success state is ONE message for both
// outcomes (the address has an account / it doesn't). With the hub's unified
// response there is no outcome signal client-side, and the copy must not imply
// the hub emailed anyone: the copyable /invite link is the centerpiece.

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ShareForm from "./ShareForm";

vi.mock("../api", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../api")>();
  return { ...mod, createShare: vi.fn() };
});
import { createShare } from "../api";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

function setInputValue(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("ShareForm success copy (proposal 0056 C2)", () => {
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

  async function share(email: string): Promise<string> {
    await act(async () =>
      root.render(
        <ShareForm subject={{ title: "laptop", machine: "laptop" }} onClose={() => {}} />
      )
    );
    const input = container.querySelector("input[type=email]") as HTMLInputElement;
    await act(async () => setInputValue(input, email));
    const form = container.querySelector("form")!;
    await act(async () => {
      form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    return container.innerHTML;
  }

  it("shows the identical success copy for a known-account invite and an email invite", async () => {
    // Outcome 1: the address has an account (server still answers the unified shape).
    vi.mocked(createShare).mockResolvedValue({ id: "i1", status: "pending", inviteUrl: "/invite/tok-a" });
    const known = await share("bob@x.com");

    await act(async () => root.unmount());
    root = createRoot(container);

    // Outcome 2: no account yet — same shape, different token.
    vi.mocked(createShare).mockResolvedValue({ id: "i2", status: "pending", inviteUrl: "/invite/tok-b" });
    const unknown = await share("ghost@x.com");

    for (const html of [known, unknown]) {
      expect(html).toContain("Invitation created for");
      expect(html).toContain("ll see it when they sign in");
      expect(html).toContain("You can also send them this link:");
      expect(html).toContain("Copy link");
      // Nothing implies the hub delivered an email.
      expect(html).not.toContain("Invitation sent");
    }
    // The two renders differ only in the email + token — normalize and compare,
    // proving the copy genuinely doesn't branch on the outcome.
    const normalize = (html: string, email: string, tok: string) =>
      html.replaceAll(email, "EMAIL").replaceAll(tok, "TOKEN");
    expect(normalize(known, "bob@x.com", "tok-a")).toBe(normalize(unknown, "ghost@x.com", "tok-b"));
  });

  it("renders the resolved absolute invite link with a copy button", async () => {
    vi.mocked(createShare).mockResolvedValue({ id: "i3", status: "pending", inviteUrl: "/invite/tok-c" });
    const html = await share("someone@x.com");
    expect(html).toContain("/invite/tok-c");
    expect(html).toContain("Copy link");
  });
});
