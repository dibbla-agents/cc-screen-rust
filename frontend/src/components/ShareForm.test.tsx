// Proposal 0056 C2 + 0073 D2/D3 — the share form's success state is ONE message
// for both outcomes (the address has an account / it doesn't). The copy has two
// variants now, but the switch is the hub-wide `mail` capability, never anything
// about the invitee: with no mailer the wording is byte-for-byte what it always
// was, with one it says (in the present progressive — the send is spawned and
// hasn't happened yet) that the invite is being emailed. In both, the copyable
// /invite link keeps its prominence, because it is the fallback for a bounce, a
// spam folder and every hub that sends nothing at all. The normalize-and-compare
// at the bottom of the first test is the frontend's copy of the no-oracle rule:
// if a delivery indicator ever breaks it, the indicator is wrong.

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

// The same suite runs against both hub shapes: `mail: false` is a self-hosted
// hub with no relay (and every pre-0073 render), `mail: true` is one with
// CCHUB_SMTP_URL configured.
describe.each([{ mail: false }, { mail: true }])(
  "ShareForm success copy (proposal 0056 C2 / 0073 D2), mail=$mail",
  ({ mail }) => {
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
          <ShareForm
            subject={{ title: "laptop", machine: "laptop" }}
            onClose={() => {}}
            mail={mail}
          />
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
        // The link is the centerpiece on BOTH hubs.
        expect(html).toContain("Copy link");
        if (mail) {
          // 0073 D2: present progressive, and the link demoted to a fallback —
          // never a past-tense claim the response cannot honestly make.
          expect(html).toContain("emailing them, and they");
          expect(html).toContain("arrive, send them this link:");
          expect(html).not.toContain("we emailed them");
          expect(html).not.toContain("You can also send them this link:");
        } else {
          // Byte-for-byte today's sentence; nothing implies the hub delivered
          // an email, because on this hub nothing did.
          expect(html).toContain("You can also send them this link:");
          expect(html).not.toContain("Invitation sent");
          expect(html).not.toContain("emailing");
        }
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
  }
);
