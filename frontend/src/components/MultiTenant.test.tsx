// Proposal 0056 — the LimitCard (Part B) and the post-activation hand-off
// (Part A1). The LimitCard assertions are static renders; the ActivatePage
// flows drive the real component against a mocked api module (the "mocked 402"
// approve path and the success path offering "Start your first session").
// Proposal 0073 D3 adds the first TeamInviteForm coverage (both `me.mail`
// states) and pins the seats LimitCard's "sent again" copy, plus the pure
// delivery-badge mapping behind the pending-invite rows.

import { renderToStaticMarkup } from "react-dom/server";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ActivatePage,
  LimitCard,
  TeamInviteForm,
  canResendInvite,
  deliveryBadge,
} from "./MultiTenant";
import type { MeInfo, MePlan } from "../api";

vi.mock("../api", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../api")>();
  return {
    ...mod,
    approveDevice: vi.fn(),
    listAgents: vi.fn().mockResolvedValue([]),
    createOrgInvite: vi.fn(),
  };
});
import { approveDevice, createOrgInvite, listAgents } from "../api";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const plan: MePlan = { name: "free", maxAgents: 10, maxSessions: 50, agents: 10 };

describe("LimitCard (proposals 0056 Part B / 0058 C2)", () => {
  it("renders the mailto-primary card on a self-hosted hub (billing off)", () => {
    const html = renderToStaticMarkup(<LimitCard plan={plan} what="machines" support="erik@dibbla.com" />);
    expect(html).toContain("Plan limit reached");
    expect(html).toContain("free");
    expect(html).toContain("10");
    expect(html).toContain("machines");
    expect(html).toContain("Unlink a machine");
    expect(html).toContain("mailto:erik@dibbla.com?subject=cc-screen%20plan%20upgrade%20(machines)");
    expect(html).toContain("Request an upgrade");
    // No checkout on a hub without Stripe configured.
    expect(html).not.toContain("Upgrade to Pro");
  });

  it("renders a checkout button (mailto demoted) on a billing-enabled hub", () => {
    const html = renderToStaticMarkup(
      <LimitCard plan={plan} what="machines" support="erik@dibbla.com" billing />
    );
    expect(html).toContain("Upgrade to Pro");
    expect(html).toContain("Nothing is deleted at a limit");
    // The mailto survives as a quiet secondary "Questions? Email us" link.
    expect(html).toContain("Questions? Email us");
    expect(html).toContain("mailto:erik@dibbla.com");
  });

  it("offers the founder price to a beta user inside the window (billing on)", () => {
    const beta: MePlan = { name: "beta", maxAgents: 10, maxSessions: 50, agents: 10 };
    const html = renderToStaticMarkup(<LimitCard plan={beta} what="machines" billing />);
    expect(html).toContain("$5/mo founder price");
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

  // Proposal 0073 D3: the seats card's reassurance reads differently once
  // "sent" means something, so pin it. It is the one LimitCard branch that was
  // never asserted.
  it("renders the seats card with the 'sent again' reassurance", () => {
    const teamPlan: MePlan = {
      name: "team",
      maxAgents: 50,
      maxSessions: 200,
      agents: 3,
      seats: 3,
      members: 3,
      orgId: "o1",
      orgRole: "owner",
    };
    const html = renderToStaticMarkup(
      <LimitCard plan={teamPlan} what="seats" support="erik@dibbla.com" billing />
    );
    expect(html).toContain("Plan limit reached");
    expect(html).toContain("seats are all in use");
    expect(html).toContain("Add seats");
    expect(html).toContain("pending invites can simply be sent again");
    // The seats branch is its own card — never the machines/sessions copy.
    expect(html).not.toContain("Unlink a machine");

    // A plain member sees the owner's address instead of the portal button.
    const member = renderToStaticMarkup(
      <LimitCard
        plan={{ ...teamPlan, orgRole: "member", ownerEmail: "boss@x.com" }}
        what="seats"
        billing
      />
    );
    expect(member).toContain("boss@x.com");
    expect(member).not.toContain("Add seats");
  });
});

// ── Delivery state (proposal 0073 D2) ────────────────────────────────────────

describe("invite delivery badge (proposal 0073 D2)", () => {
  it("maps every delivery state, with NULL staying today's 'invited'", () => {
    expect(deliveryBadge(null)).toBe("invited");
    expect(deliveryBadge(undefined)).toBe("invited");
    expect(deliveryBadge("sending")).toBe("sending");
    expect(deliveryBadge("sent")).toBe("sent");
    expect(deliveryBadge("failed")).toBe("failed");
    expect(deliveryBadge("rejected")).toBe("bad address");
    // A newer hub's unknown value degrades to the pre-mailer rendering rather
    // than showing a raw token.
    expect(deliveryBadge("quarantined")).toBe("invited");
  });

  it("offers Resend for a transient failure only — never for a permanent refusal", () => {
    expect(canResendInvite("failed")).toBe(true);
    // `rejected` is permanent: retrying cannot succeed and spends send quota.
    expect(canResendInvite("rejected")).toBe(false);
    expect(canResendInvite("sent")).toBe(false);
    expect(canResendInvite("sending")).toBe(false);
    expect(canResendInvite(null)).toBe(false);
    expect(canResendInvite(undefined)).toBe(false);
  });
});

// ── TeamInviteForm success copy, both hub shapes (proposal 0073 D2/D3) ───────

describe("TeamInviteForm success copy (proposal 0073 D2)", () => {
  let container: HTMLDivElement;
  let root: Root;

  const me = (mail: boolean): MeInfo => ({
    multiTenant: true,
    googleEnabled: false,
    authenticated: true,
    email: "admin@x.com",
    mail,
  });

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

  async function invite(mail: boolean): Promise<string> {
    vi.mocked(createOrgInvite).mockResolvedValue({
      id: "i1",
      status: "pending",
      inviteUrl: "/org-invite/tok-a",
    });
    await act(async () => root.render(<TeamInviteForm me={me(mail)} onInvited={() => {}} />));
    const input = container.querySelector("input[type=email]") as HTMLInputElement;
    await act(async () => setInputValue(input, "new@x.com"));
    const form = container.querySelector("form")!;
    await act(async () => {
      form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    return container.innerHTML;
  }

  it("keeps today's sentence byte-for-byte on a hub with no mailer", async () => {
    const html = await invite(false);
    expect(html).toContain("Invitation created for");
    expect(html).toContain("new@x.com");
    expect(html).toContain("ll see it when they sign in");
    expect(html).toContain("You can also send them this link:");
    expect(html).not.toContain("emailing");
    // The link is the ONLY channel here, so it had better be there.
    expect(html).toContain("/org-invite/tok-a");
    expect(html).toContain("Copy link");
  });

  it("says the invite is being emailed (present progressive) on a mail hub, link intact", async () => {
    const html = await invite(true);
    expect(html).toContain("Invitation created for");
    expect(html).toContain("emailing them, and they");
    expect(html).toContain("ll see it when they sign in");
    expect(html).toContain("arrive, send them this link:");
    // The send is spawned after the response — no past-tense claim is honest yet.
    expect(html).not.toContain("we emailed them");
    expect(html).not.toContain("Invitation sent");
    // Same prominence for the fallback link on both hubs.
    expect(html).toContain("/org-invite/tok-a");
    expect(html).toContain("Copy link");
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
