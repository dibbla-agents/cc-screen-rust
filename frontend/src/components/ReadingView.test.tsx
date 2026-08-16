import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import ReadingView from "./ReadingView";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// Proposal 0083 Part C — the rendering invariant, pinned the same way [0077]
// pins osc52's structural refusal.
//
// The acceptance criterion is "this module cannot render HTML", not "we
// remembered to sanitise": react-markdown's defaults ARE the boundary (no raw
// HTML, `javascript:`/`data:` hrefs stripped by its URL transform), and every
// realistic way to lose them is an import or an attribute someone adds later.
// So this reads the source and asserts their absence. It matters because the
// `/s/<token>` page renders UNTRUSTED prose on the product's own origin, where
// executing anything is a phishing and XSS problem for the whole product, not
// just for one shared file.
describe("the reading view renders untrusted markdown as text", () => {
  const raw = readFileSync(join(process.cwd(), "src", "components", "ReadingView.tsx"), "utf8");
  // Comments are prose about the invariant; the invariant is the code.
  const src = raw.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");

  it("pulls in no HTML-passthrough plugin", () => {
    for (const forbidden of ["rehype-raw", "rehypeRaw", "rehype-sanitize", "remark-html", "rehype-stringify"]) {
      expect(src).not.toContain(forbidden);
    }
    // Whatever it does import, none of it may be a rehype plugin: a rehype
    // pipeline is the only way raw HTML gets through react-markdown.
    expect(/from\s+["']rehype/.test(src)).toBe(false);
  });

  it("never injects markup", () => {
    for (const forbidden of ["dangerouslySetInnerHTML", "innerHTML", "createElement(", "document.write"]) {
      expect(src).not.toContain(forbidden);
    }
  });

  it("keeps react-markdown's default URL transform", () => {
    // Overriding `urlTransform` / the legacy `transformLinkUri` is the one
    // supported way to re-enable `javascript:` hrefs.
    expect(src).not.toContain("urlTransform");
    expect(src).not.toContain("transformLinkUri");
    expect(src).not.toContain("transformImageUri");
  });

  it("only overrides `pre` and `li` — no anchor or image component to smuggle a URL through", () => {
    const componentKeys = [...src.matchAll(/^\s{4}(\w+):/gm)].map((m) => m[1]);
    expect(componentKeys.sort()).toEqual(["li", "pre"]);
  });
});

// The same invariant, from the other side: render the hostile fixture and
// assert nothing became markup. The source pin above catches the imports; this
// catches a react-markdown default changing under us.
describe("hostile markdown renders as text, not as markup", () => {
  let host: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
  });
  afterEach(() => {
    act(() => root.unmount());
    host.remove();
  });

  const HOSTILE = [
    "# Shared notes",
    "",
    "<script>window.__pwned = 1</script>",
    "",
    '<img src="x" onerror="window.__pwned = 1">',
    "",
    '<div id="raw-html-block">raw block</div>',
    "",
    "[click me](javascript:window.__pwned=1)",
    "",
    "[data uri](data:text/html;base64,PHNjcmlwdD53aW5kb3cuX19wd25lZD0xPC9zY3JpcHQ+)",
    "",
    "- [ ] a task nobody can toggle here",
  ].join("\n");

  it("executes nothing and injects no elements", () => {
    act(() => root.render(<ReadingView content={HOSTILE} />));
    // The prose renders — this is not a blank page passing by accident.
    expect(host.querySelector("h1")?.textContent).toBe("Shared notes");
    // No raw HTML became elements.
    expect(host.querySelector("script")).toBeNull();
    expect(host.querySelector("img")).toBeNull();
    expect(host.querySelector("#raw-html-block")).toBeNull();
    expect((globalThis as { __pwned?: number }).__pwned).toBeUndefined();
    // The raw HTML is visible as TEXT, which is exactly what it should be.
    expect(host.textContent).toContain("<script>");
    // No executable href survived the default URL transform.
    for (const a of host.querySelectorAll("a")) {
      const href = a.getAttribute("href") ?? "";
      expect(href.toLowerCase().startsWith("javascript:")).toBe(false);
      expect(href.toLowerCase().startsWith("data:")).toBe(false);
    }
  });

  it("renders task checkboxes inert when no toggle handler is given", () => {
    act(() => root.render(<ReadingView content={"- [ ] one\n- [x] two\n"} />));
    // Not a disabled-looking control with a live handler: there is no button at
    // all — [0030]'s write path is simply absent from a read-only surface.
    expect(host.querySelectorAll("button[role=checkbox]").length).toBe(0);
    expect(host.querySelectorAll(".cc-task-checkbox").length).toBe(2);
  });

  it("still renders an interactive checkbox when the editor passes a handler", () => {
    const seen: number[] = [];
    act(() => root.render(<ReadingView content={"- [ ] one\n"} onToggleTask={(o) => seen.push(o)} />));
    const box = host.querySelector<HTMLButtonElement>("button[role=checkbox]");
    expect(box).not.toBeNull();
    act(() => {
      box!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(seen.length).toBe(1);
  });
});
