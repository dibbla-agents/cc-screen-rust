import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import SessionDrawer, { actionMatchLen, residualQuery } from "./SessionDrawer";
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

// Proposal 0087 — the per-session Restart button. Restarting an assistant so it
// re-reads a launch-time config (a newly added MCP server is the canonical case)
// used to have no non-destructive path: `/exit` makes the reaper forget the
// session, Delete forgets it on purpose, and [0049]'s action restarts every
// same-tool session on the machine.
describe("SessionDrawer restart action (proposal 0087)", () => {
  const rows = [session({ name: "alpha" }), session({ name: "term", tool: "shell" })];
  const withRestart = {
    ...baseProps,
    sessions: rows,
    onRestart: () => {},
    restarting: new Set<string>(),
  };

  it("offers restart on an assistant row and never on a shell row", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...withRestart} pane open current={null} keyboardActive />
    );
    // C1: the affordance lives in the row action cluster, beside Delete.
    expect(html).toContain('aria-label="Restart session alpha"');
    // C5: `shell` has no conversation to resume — the engine refuses it
    // structurally, so the UI must not render a dead affordance.
    expect(html).not.toContain('aria-label="Restart session term"');
  });

  it("renders nothing when the host wires no onRestart", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...baseProps} pane open current={null} keyboardActive />
    );
    expect(html).not.toContain("Restart session");
  });

  it("arms an inline confirm and fires only on the second press", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    const fired: string[] = [];
    await act(async () => {
      root.render(
        <SessionDrawer
          {...withRestart}
          onRestart={(s) => fired.push(s.name)}
          pane
          open
          current={null}
          keyboardActive={false}
        />
      );
    });

    const arm = container.querySelector<HTMLButtonElement>('[aria-label="Restart session alpha"]');
    expect(arm).toBeTruthy();
    // First press only ARMS — no window.confirm anywhere, per the [0049]
    // F-series rule, and nothing has been restarted yet.
    await act(async () => arm!.click());
    expect(fired).toEqual([]);
    const confirm = container.querySelector<HTMLButtonElement>("[data-restart-confirm]");
    expect(confirm, "the confirm pair replaces the cluster in place").toBeTruthy();
    // A ready session carries no busy warning (C3).
    expect(container.querySelector("[data-restart-busy]")).toBeNull();

    await act(async () => confirm!.click());
    expect(fired).toEqual(["alpha"]);
    // …and the confirm disarms itself.
    expect(container.querySelector("[data-restart-confirm]")).toBeNull();

    await act(async () => root.unmount());
    container.remove();
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("warns in the confirm state when the agent looks busy — a soft gate, not a block", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(
        <SessionDrawer
          {...withRestart}
          sessions={[session({ name: "alpha", waiting: false })]}
          pane
          open
          current={null}
          keyboardActive={false}
        />
      );
    });
    await act(async () =>
      container.querySelector<HTMLButtonElement>('[aria-label="Restart session alpha"]')!.click()
    );
    const warn = container.querySelector("[data-restart-busy]");
    expect(warn?.textContent).toContain("may lose the last exchange");
    // The action stays available — the user may be restarting BECAUSE it wedged.
    expect(container.querySelector("[data-restart-confirm]")).toBeTruthy();

    await act(async () => root.unmount());
    container.remove();
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("swaps the cluster for a spinner while a restart is in flight", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer
        {...withRestart}
        restarting={new Set(["alpha"])}
        pane
        open
        current={null}
        keyboardActive
      />
    );
    expect(html).toContain("data-restarting");
    expect(html).not.toContain('aria-label="Restart session alpha"');
  });
});

// ── Proposal 0086 ─────────────────────────────────────────────────────────────
// CreateSession fetches tools + dirs on mount; neither result matters to these
// tests, so one stub answers both.
const withStubbedApi = async (fn: () => Promise<void>) => {
  const realFetch = globalThis.fetch;
  // jsdom has no layout, so it ships no scrollIntoView; the cursor effects call it.
  if (!HTMLElement.prototype.scrollIntoView) HTMLElement.prototype.scrollIntoView = () => {};
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    const body = url.includes("/api/tools")
      ? [{ cmd: "cc", label: "Claude" }]
      : url.includes("/api/dirs/search")
        ? { root: "/home/u", home: "/home/u", results: [] }
        : { path: "/home/u", home: "/home/u", atHome: true, parent: "/home", dirs: [] };
    return { ok: true, status: 200, json: async () => body } as unknown as Response;
  }) as typeof fetch;
  try {
    await fn();
  } finally {
    globalThis.fetch = realFetch;
  }
};

// Type into a controlled React input the way a user would (React listens for
// the `input` event and reads the value the native setter wrote).
const typeInto = (el: HTMLInputElement, value: string) => {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
};

// Part A: the typed query that summoned "New session" is the action's own name,
// not a folder — so it must not arrive in the create panel as a folder filter
// (where nothing matched it, the only row was `Create folder "new"`, and a
// reflexive second ⏎ minted a `~/new` directory).
describe("actionMatchLen / residualQuery (proposal 0086 A1)", () => {
  it("consumes an exact alias, and the whole multi-word one", () => {
    expect(actionMatchLen("new")).toBe(1);
    expect(actionMatchLen("create")).toBe(1);
    expect(actionMatchLen("start")).toBe(1);
    expect(actionMatchLen("new session")).toBe(2);
    expect(residualQuery("new")).toBe("");
    expect(residualQuery("New Session")).toBe("");
  });

  it("consumes a fuzzy fragment of an alias word", () => {
    expect(actionMatchLen("nw")).toBe(1);
    expect(actionMatchLen("crt")).toBe(1);
    expect(residualQuery("nw")).toBe("");
  });

  it("keeps the residue — [0016]'s power path", () => {
    expect(residualQuery("new myproj")).toBe("myproj");
    expect(residualQuery("create billing")).toBe("billing");
    expect(residualQuery("new session billing hub")).toBe("billing hub");
    expect(actionMatchLen("new myproj")).toBe(1);
  });

  it("never strips a query that isn't the action's name", () => {
    expect(actionMatchLen("myproj")).toBe(0);
    expect(residualQuery("myproj")).toBe("myproj");
    expect(residualQuery("darktide rust")).toBe("darktide rust");
  });

  it("does NOT strip an alias that is the prefix of a real word", () => {
    // `news` may well be a folder. The plain fuzzy scorer WOULD match it
    // against "new session" (the `s` lands in "session") — which is exactly why
    // the consumption matcher is word-aligned and not `fuzzyScore`.
    expect(actionMatchLen("news")).toBe(0);
    expect(residualQuery("news")).toBe("news");
    expect(residualQuery("starter")).toBe("starter");
    expect(residualQuery("created")).toBe("created");
  });

  it("is a no-op on an empty / whitespace query", () => {
    expect(actionMatchLen("")).toBe(0);
    expect(actionMatchLen("   ")).toBe(0);
    expect(residualQuery("   ")).toBe("");
  });

  it("labels the row with what it will carry, not with what was typed", () => {
    const html = renderToStaticMarkup(
      <SessionDrawer {...baseProps} pane open current={null} keyboardActive />
    );
    // Resting: the plain action row (no query to carry).
    expect(html).toContain("New session…");
  });

  // The end-to-end shape of the footgun, at component level: type the action's
  // name, activate the row, and the panel must open CLEAN — no folder filter,
  // no auto-derived name, hence no lone `Create folder "new"` row for a second
  // ⏎ to act on.
  const enterCreateAfterTyping = async (typed: string) => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(
        <SessionDrawer
          {...baseProps}
          createInitialMachine=""
          pane
          open
          current={null}
          keyboardActive={false}
        />
      );
    });
    const filter = container.querySelector<HTMLInputElement>('input[placeholder^="Search sessions"]');
    await act(async () => typeInto(filter!, typed));
    const newRow = [...container.querySelectorAll("button")].find((b) =>
      (b.textContent ?? "").startsWith("New session")
    );
    expect(newRow, `no "New session" row for ${JSON.stringify(typed)}`).toBeTruthy();
    await act(async () => newRow!.click());
    return { container, root };
  };

  it("opens the create panel with an empty folder filter and name after `new` ⏎", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const { container, root } = await enterCreateAfterTyping("new");
      expect(container.querySelector<HTMLInputElement>("[data-create-search]")!.value).toBe("");
      expect(container.querySelector<HTMLInputElement>("[data-create-name]")!.value).toBe("");
      // …and with no query there is no mkdir row to fire on a second ⏎.
      expect(container.textContent).not.toContain("Create folder");
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("still carries the residue after the action term ([0016]'s power path)", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const { container, root } = await enterCreateAfterTyping("new myproj");
      expect(container.querySelector<HTMLInputElement>("[data-create-search]")!.value).toBe(
        "myproj"
      );
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("carries a non-action query whole — the rule only ever strips the action's name", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const { container, root } = await enterCreateAfterTyping("news");
      expect(container.querySelector<HTMLInputElement>("[data-create-search]")!.value).toBe("news");
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });
});

// Part B: the create panel's machine. `createMachines` is App's MRU-ordered
// view of the roster; the panel renders it verbatim and preselects
// `createInitialMachine`, which App derives from the same order.
describe("CreateSession machine order (proposal 0086 B2/B3)", () => {
  const machines = [
    { machine: "alpha", hostname: "alpha.local", online: true },
    { machine: "bravo", hostname: "bravo.local", online: true },
  ];

  const openCreate = async (props: Record<string, unknown>) => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(
        <SessionDrawer
          {...baseProps}
          {...props}
          machines={machines}
          multiMachine
          pane
          open
          current={null}
          keyboardActive={false}
        />
      );
    });
    // Click the "New session" action row to enter create mode.
    const rows = [...container.querySelectorAll("button")];
    const newRow = rows.find((b) => (b.textContent ?? "").startsWith("New session"));
    await act(async () => newRow!.click());
    return { container, root };
  };

  it("preselects the MRU machine and lists the roster in MRU order", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      // App's derivation said "bravo" even though the roster leads with alpha.
      const { container, root } = await openCreate({
        createInitialMachine: "bravo",
        createMachines: [machines[1], machines[0]],
      });
      const sel = container.querySelector<HTMLSelectElement>("[data-create-machine]");
      expect(sel).toBeTruthy();
      expect(sel!.value).toBe("bravo");
      expect([...sel!.options].map((o) => o.value)).toEqual(["bravo", "alpha"]);
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("falls back to the roster order when no MRU list is supplied", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const { container, root } = await openCreate({ createInitialMachine: "alpha" });
      const sel = container.querySelector<HTMLSelectElement>("[data-create-machine]");
      expect([...sel!.options].map((o) => o.value)).toEqual(["alpha", "bravo"]);
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("a [0056] seed still beats MRU — the panel opens on the seeded machine", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const container = document.createElement("div");
      document.body.appendChild(container);
      const root = createRoot(container);
      await act(async () => {
        root.render(
          <SessionDrawer
            {...baseProps}
            machines={machines}
            multiMachine
            createInitialMachine="bravo"
            createMachines={[machines[1], machines[0]]}
            createSeed={{ machine: "alpha" }}
            pane
            open
            current={null}
            keyboardActive={false}
          />
        );
      });
      const sel = container.querySelector<HTMLSelectElement>("[data-create-machine]");
      expect(sel!.value).toBe("alpha");
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });

  it("an empty seed ({}) opens create mode with no machine override (⌃B n)", async () => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    await withStubbedApi(async () => {
      const container = document.createElement("div");
      document.body.appendChild(container);
      const root = createRoot(container);
      await act(async () => {
        root.render(
          <SessionDrawer
            {...baseProps}
            machines={machines}
            multiMachine
            createInitialMachine="bravo"
            createMachines={[machines[1], machines[0]]}
            createSeed={{}}
            pane
            open
            current={null}
            keyboardActive={false}
          />
        );
      });
      const sel = container.querySelector<HTMLSelectElement>("[data-create-machine]");
      expect(sel, "an empty seed still enters create mode").toBeTruthy();
      expect(sel!.value).toBe("bravo"); // the MRU default, not a seeded machine
      // …and the folder search opens empty.
      const search = container.querySelector<HTMLInputElement>("[data-create-search]");
      expect(search!.value).toBe("");
      await act(async () => root.unmount());
      container.remove();
    });
    globalThis.IS_REACT_ACT_ENVIRONMENT = false;
  });
});
