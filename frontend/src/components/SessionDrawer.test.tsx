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

// Proposal 0032 — every switcher row reads name (row 1) → folder breadcrumb/path
// (row 2) → summary (row 3), in every variant (pane, mobile, and the desktop
// sidebar — consistent everywhere). The name (`s.short`) is the bright, leading
// element and always present (even with no cwd); the breadcrumb sits on the
// second line. Static-render structural assertions, matching 0026's style.
describe("SessionDrawer — name-on-top row (proposal 0032)", () => {
  // A session whose name differs from the cwd leaf, so the name row and the
  // breadcrumb leaf are two distinct strings we can assert on independently.
  const withCwd = {
    sessions: [session({ name: "auth-work", short: "auth-work", cwd: "/home/erik/development/cc-screen-rust" })],
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

  it("renders the name as a distinct bright top element and the breadcrumb leaf on a second line", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...withCwd} pane open current={null} keyboardActive />
    );
    // Row 1 — the name (`s.short`), bright/semibold, leading.
    expect(html).toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">auth-work</span>'
    );
    // Row 2 — the breadcrumb: parent dim + leaf bright, two separate nodes.
    expect(html).toContain('<span class="truncate text-slate-500">development</span>');
    expect(html).toContain(
      '<span class="shrink-0 truncate text-slate-100">cc-screen-rust</span>'
    );
    // The name and the breadcrumb leaf are genuinely distinct strings here.
    expect(html).toContain("auth-work");
    expect(html).toContain("cc-screen-rust");
  });

  it("sidebar variant also leads with the name, breadcrumb on a second row (consistent with pane)", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...withCwd} sidebar open current={null} keyboardActive />
    );
    // Row 1 — the name leads in the sidebar too (same markup as the pane).
    expect(html).toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">auth-work</span>'
    );
    // Row 2 — the breadcrumb moves to the `mt-0.5` path row, not the top line.
    expect(html).toContain(
      '<span class="mt-0.5 flex min-w-0 items-baseline text-[13px] font-medium">'
    );
    expect(html).toContain('<span class="truncate text-slate-500">development</span>');
    expect(html).toContain(
      '<span class="shrink-0 truncate text-slate-100">cc-screen-rust</span>'
    );
  });

  it("no-cwd session keeps the name row but omits the path row (pane)", () => {
    const noCwd = { ...withCwd, sessions: [session({ name: "scratch", short: "scratch" })] };
    const html = renderToStaticMarkup(
      <SessionDrawer {...noCwd} pane open current={null} keyboardActive />
    );
    // Name row still present and leading.
    expect(html).toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">scratch</span>'
    );
    // No breadcrumb path row — nothing to show without a cwd.
    expect(html).not.toContain(
      '<span class="mt-0.5 flex min-w-0 items-baseline text-[13px] font-medium">'
    );
  });
});
