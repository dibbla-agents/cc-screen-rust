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
  onRename: () => {},
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
// Proposal 0068 Part B — a CLOSED sidebar drawer keeps its root (so the 200ms
// slide still animates) but renders no session rows: an off-screen list of rows
// is invisible work, and each running session's dot used to keep painting there.
describe("SessionDrawer closed-drawer row gating (proposal 0068)", () => {
  it("renders zero session rows while closed, and the rows when open", () => {
    const closed = renderToStaticMarkup(
      <SessionDrawer {...baseProps} sidebar open={false} current={null} />
    );
    expect(closed).toContain('data-drawer="closed"');
    expect(closed).not.toContain("data-session-row");
    expect(closed).not.toContain("alpha");
    // The header (and with it NotificationsButton's push probe) stays mounted.
    expect(closed).toContain("Sessions");

    const open = renderToStaticMarkup(
      <SessionDrawer {...baseProps} sidebar open current={null} />
    );
    expect(open).toContain("data-session-row");
    expect(open).toContain("alpha");
  });
});

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
    onRename: () => {},
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

// Proposal 0035 — a session's display label overrides `short` on the name row
// (the identity `short` stays the routing key underneath). Static-render
// structural assertions, matching the style above.
describe("SessionDrawer — display label (proposal 0035)", () => {
  it("renders the label in place of the slug on the name row", () => {
    const labelled = {
      ...baseProps,
      sessions: [session({ name: "claude-x", short: "claude-x", label: "Auth refactor" })],
    };
    const html = renderToStaticMarkup(
      <SessionDrawer {...labelled} pane open current={null} keyboardActive />
    );
    // The name row shows the label, not the slug.
    expect(html).toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">Auth refactor</span>'
    );
    expect(html).not.toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">claude-x</span>'
    );
  });

  it("falls back to the slug when the label is empty/whitespace", () => {
    const blank = {
      ...baseProps,
      sessions: [session({ name: "claude-y", short: "claude-y", label: "   " })],
    };
    const html = renderToStaticMarkup(
      <SessionDrawer {...blank} pane open current={null} keyboardActive />
    );
    expect(html).toContain(
      '<span class="truncate text-[13px] font-semibold text-slate-100">claude-y</span>'
    );
  });
});

// Proposal 0056 A3/D — the no-sessions empty state is build-aware: a hub user
// is pointed at "New session" (a real button), never at running `cc` on the
// box; single-tenant keeps the cc hint and gains the docs link (Part D).
describe("SessionDrawer — build-aware empty state (proposal 0056)", () => {
  const empty = { ...baseProps, sessions: [] as Session[] };

  it("multi-tenant copy names New session and drops the cc-on-the-box hint", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...empty} multiTenant pane open current={null} keyboardActive />
    );
    expect(html).toContain("No sessions yet.");
    expect(html).toContain("pick a machine,");
    expect(html).toContain("an assistant, and a folder");
    expect(html).not.toContain("on the box");
    expect(html).not.toContain("ccscreen.dev/docs");
  });

  it("single-tenant keeps the cc hint and gains the docs link", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...empty} pane open current={null} keyboardActive />
    );
    expect(html).toContain("No sessions yet.");
    expect(html).toContain("on the box");
    expect(html).toContain("https://ccscreen.dev/docs");
    expect(html).not.toContain("pick a machine,");
  });
});

// Proposal 0078 — the `Recent` section: the sessions you were last working in,
// lifted above the machine groups in strict MRU order. The load-bearing claim
// is *positional stability* — these rows must not move when an agent changes
// state — so the criterion-2 test below renders twice with the attention inputs
// flipped and asserts the order is byte-identical.
describe("SessionDrawer — the Recent section (proposal 0078)", () => {
  const three = [
    session({ name: "alpha", machine: "pine", cwd: "/home/erik/a" }),
    session({ name: "beta", machine: "pine", cwd: "/home/erik/b" }),
    session({ name: "gamma", machine: "studio", cwd: "/home/erik/g" }),
  ];
  const props = {
    ...baseProps,
    sessions: three,
    machines: [
      { machine: "pine", hostname: "pine", online: true },
      { machine: "studio", hostname: "studio", online: true },
    ],
    multiMachine: true,
    recents: [
      { machine: "studio", name: "gamma" },
      { machine: "pine", name: "alpha" },
    ],
    mountedKeys: new Set<string>(),
  };
  const order = (html: string, ...names: string[]) => names.map((n) => html.indexOf(n));
  const sectionRows = (html: string) =>
    [...html.matchAll(/data-session-row=""\s*data-recent-section=""[\s\S]*?<\/span>/g)].length;

  it("puts the section, in MRU order, above the first machine header", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...props} sidebar open current={null} keyboardActive />
    );
    expect(html).toContain("Recent");
    const [recent, gamma, alpha, firstHeader] = order(
      html,
      "Recent",
      "gamma",
      "alpha",
      'tracking-wider text-slate-500">pine</span>' // the machine group header
    );
    expect(recent).toBeLessThan(gamma);
    expect(gamma).toBeLessThan(alpha); // MRU order, not triage order
    expect(alpha).toBeLessThan(firstHeader); // the whole section precedes the groups
  });

  it("does not reorder when an agent goes ready or busy (criterion 2)", () => {
    const flip = (waiting: boolean) => ({
      ...props,
      sessions: [
        { ...three[0], waiting, busy_until: waiting ? 9_000 : 0, activity: 9_000 },
        three[1],
        { ...three[2], waiting: !waiting, activity: 1 },
      ],
    });
    const a = renderToStaticMarkup(
      <SessionDrawer {...flip(true)} sidebar open current={null} keyboardActive />
    );
    const b = renderToStaticMarkup(
      <SessionDrawer {...flip(false)} sidebar open current={null} keyboardActive />
    );
    // The section is MRU: gamma before alpha in both renders, whatever the
    // agents are doing. This test is what fails if anyone ever "improves" the
    // section by attention-ordering it.
    for (const html of [a, b]) {
      const [g, al] = order(html, "gamma", "alpha");
      expect(g).toBeLessThan(al);
    }
  });

  it("renders each session exactly once — section members leave their group", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...props} sidebar open current={null} keyboardActive />
    );
    expect([...html.matchAll(/>gamma</g)]).toHaveLength(1);
    expect([...html.matchAll(/>alpha</g)]).toHaveLength(1);
    // studio's only session is in the section, so studio renders no group header
    // and no empty group; pine still renders its header for beta.
    const groupHeader = (host: string) =>
      html.includes(`tracking-wider text-slate-500">${host}</span>`);
    expect(groupHeader("pine")).toBe(true);
    expect(groupHeader("studio")).toBe(false); // "studio" survives only as a row chip
  });

  it("excludes a session mounted in a pane, which keeps its row (and dot) below", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer
        {...props}
        sessions={[three[0], three[1], { ...three[2], attached: true }]}
        mountedKeys={new Set(["studio/gamma"])}
        pane
        open
        current={null}
        keyboardActive
      />
    );
    expect(sectionRows(html)).toBe(1); // alpha only
    const [recent, alpha, gamma] = order(html, "Recent", "alpha", "gamma");
    expect(recent).toBeLessThan(alpha);
    expect(alpha).toBeLessThan(gamma); // gamma is below, in its machine group
    expect(html).toContain("already shown in another pane");
  });

  it("carries a machine chip on section rows, which have no header to inherit", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...props} sidebar open current={null} keyboardActive />
    );
    const chip = /<span class="shrink-0 rounded bg-edge\/60 px-1 py-px text-\[9px\] text-slate-400">(\w+)<\/span>/g;
    expect([...html.matchAll(chip)].map((m) => m[1])).toEqual(["studio", "pine"]);
  });

  it("suppresses the section when it would hold every session (A7)", () => {
    const all = {
      ...props,
      recents: three.map((s) => ({ machine: s.machine!, name: s.name })),
    };
    const html = renderToStaticMarkup(
      <SessionDrawer {...all} sidebar open current={null} keyboardActive />
    );
    expect(html).not.toContain(">Recent<");
    const [pineHdr, alpha] = order(html, 'tracking-wider text-slate-500">pine</span>', "alpha");
    expect(pineHdr).toBeLessThan(alpha); // today's grouped list, unchanged
  });

  it("renders no section with zero eligible recents, and none while filtering", () => {
    const none = renderToStaticMarkup(
      <SessionDrawer {...props} recents={[]} sidebar open current={null} keyboardActive />
    );
    expect(none).not.toContain(">Recent<");
    expect(none).not.toContain("data-recent-section");
    // A query short-circuits the split entirely (A8) — no lifting, no header.
    const ghosts = renderToStaticMarkup(
      <SessionDrawer
        {...props}
        recents={[{ machine: "pine", name: "does-not-exist" }]}
        sidebar
        open
        current={null}
        keyboardActive
      />
    );
    expect(ghosts).not.toContain(">Recent<");
  });

  it("caps the pane-embedded section at 5 rows (B8)", () => {
    const many = Array.from({ length: 8 }, (_, i) =>
      session({ name: `s${i}`, machine: "pine", cwd: `/home/erik/s${i}` })
    );
    const html = renderToStaticMarkup(
      <SessionDrawer
        {...props}
        sessions={[...many, session({ name: "tail", machine: "pine" })]}
        recents={many.map((s) => ({ machine: "pine", name: s.name }))}
        pane
        open
        current={null}
        keyboardActive
      />
    );
    expect(sectionRows(html)).toBe(5);
  });
});
