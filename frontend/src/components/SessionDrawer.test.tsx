import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import SessionDrawer from "./SessionDrawer";
import type { Session } from "../api";

// Proposal 0026 — the empty grid pane renders the real switcher in its `pane`
// variant. There's no full component-test harness here, so this is a structural
// smoke test (static render, no effects): it pins that the pane variant shows
// the search box + actions + session rows (so it's the *full* switcher, not a
// cut-down picker) and that the app-global header chrome (Close / Refresh) is
// dropped, without yanking focus or registering keyboard handlers.

const session = (over: Partial<Session> & Pick<Session, "name">): Session => ({
  tool: "claude",
  short: over.name,
  attached: false,
  activity: 0,
  preview: "",
  waiting: true,
  ...over,
});

const baseProps = {
  sessions: [session({ name: "alpha" }), session({ name: "beta", attached: true })],
  connByRef: {},
  machines: [],
  multiMachine: false,
  loading: false,
  error: null,
  onPick: () => {},
  onClose: () => {},
  onRefresh: () => {},
  onStatus: () => {},
  onNew: () => {},
  createInitialMachine: "",
  recentDirs: [],
  onCreated: () => {},
  showLayout: true,
  onLayout: () => {},
  deleting: new Set<string>(),
  onDelete: () => {},
  restorable: [],
  onRestore: () => {},
  toastsOn: true,
  onToggleToasts: () => {},
};

describe("SessionDrawer pane variant (proposal 0026)", () => {
  it("renders the full switcher — search, New session action, and session rows", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...baseProps} pane open current={null} keyboardActive />
    );
    expect(html).toContain("Search sessions, actions"); // the search box
    expect(html).toContain("New session"); // the create action row
    expect(html).toContain("New layout"); // showLayout action
    expect(html).toContain("alpha");
    expect(html).toContain("beta");
  });

  it("drops the app-global header chrome (Close / keyboard hint) in a pane", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...baseProps} pane open current={null} keyboardActive />
    );
    expect(html).not.toContain('aria-label="Close"');
    expect(html).not.toContain("Refresh sessions");
    // The pane fills its parent in normal flow — no absolute overlay / scrim.
    expect(html).toContain("h-full w-full");
  });

  it("flags a session shown in another pane (attached) only in the pane variant", () => {
    const paneHtml = renderToStaticMarkup(
      <SessionDrawer {...baseProps} pane open current={null} keyboardActive />
    );
    expect(paneHtml).toContain("already shown in another pane");

    // The sidebar variant must stay byte-for-byte unchanged (acceptance #7) —
    // it never grows the attached badge.
    const sidebarHtml = renderToStaticMarkup(
      <SessionDrawer {...baseProps} sidebar open current={null} keyboardActive />
    );
    expect(sidebarHtml).not.toContain("already shown in another pane");
    // ...and the sidebar keeps the Close button the pane drops.
    expect(sidebarHtml).toContain('aria-label="Close"');
  });
});
