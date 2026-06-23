import { describe, it, expect } from "vitest";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { SearchQuery, getSearchQuery } from "@codemirror/search";
import { computeMatches, smartCaseSensitive, findInFileExtensions, openFind, findIsOpen, closeFind } from "./findInFile";
import { fuzzyMatchPositions } from "../util";

// computeMatches backs the "n / N" count in the find-in-file widget (0038).
describe("computeMatches", () => {
  const stateOf = (doc: string) => EditorState.create({ doc });
  const matches = (doc: string, q: ConstructorParameters<typeof SearchQuery>[0]) =>
    computeMatches(stateOf(doc), new SearchQuery(q));

  it("counts every literal occurrence, case-insensitive by default", () => {
    const m = matches("Foo foo FOO bar", { search: "foo" });
    expect(m).not.toBeNull();
    expect(m!.length).toBe(3);
  });

  it("respects case sensitivity", () => {
    const m = matches("Foo foo FOO", { search: "foo", caseSensitive: true });
    expect(m!.length).toBe(1);
    expect(m![0]).toEqual({ from: 4, to: 7 });
  });

  it("honours whole-word", () => {
    const m = matches("cat category cat", { search: "cat", wholeWord: true });
    // "category" should not count; the two standalone "cat"s should.
    expect(m!.length).toBe(2);
  });

  it("supports regex queries", () => {
    const m = matches("a1 b2 c3", { search: "[a-z]\\d", regexp: true });
    expect(m!.length).toBe(3);
  });

  it("returns null for an invalid regex (→ Invalid pattern state)", () => {
    const m = matches("anything", { search: "(unclosed", regexp: true });
    expect(m).toBeNull();
  });

  it("returns an empty list for an empty query", () => {
    expect(matches("text", { search: "" })).toEqual([]);
  });
});

// smartCaseSensitive: case follows the query until the user toggles it.
describe("smartCaseSensitive", () => {
  it("is case-insensitive for an all-lowercase query", () => {
    expect(smartCaseSensitive("foo", false, false)).toBe(false);
  });
  it("auto-enables case sensitivity when the query has an uppercase letter", () => {
    expect(smartCaseSensitive("Foo", false, false)).toBe(true);
  });
  it("uses the manual value once the user has set the toggle", () => {
    expect(smartCaseSensitive("Foo", true, false)).toBe(false);
    expect(smartCaseSensitive("foo", true, true)).toBe(true);
  });
});

// fuzzyMatchPositions backs the tree-filter substring highlight (0038, Part C).
describe("fuzzyMatchPositions", () => {
  it("returns the matched subsequence indices (greedy, left-to-right)", () => {
    // "auth.ts": a0 u1 t2 h3 .4 t5 s6 → "ath" greedily hits 0,2,3.
    expect(fuzzyMatchPositions("ath", "auth.ts")).toEqual([0, 2, 3]);
  });
  it("is case-insensitive", () => {
    expect(fuzzyMatchPositions("AT", "auth.ts")).toEqual([0, 2]);
  });
  it("returns null when not a subsequence", () => {
    expect(fuzzyMatchPositions("zzz", "auth.ts")).toBeNull();
  });
  it("returns [] for an empty query (nothing to highlight)", () => {
    expect(fuzzyMatchPositions("", "auth.ts")).toEqual([]);
  });
});

// Integration: mount a real EditorView with the find extension, open the custom
// panel, and drive it — catches runtime errors in makeFindPanel (DOM building,
// SearchCursor wiring, selection-seed) that the pure tests can't.
describe("find-in-file widget (integration)", () => {
  const mount = (doc: string, selection?: { anchor: number; head: number }) => {
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const view = new EditorView({
      state: EditorState.create({
        doc,
        selection: selection ?? undefined,
        extensions: [findInFileExtensions()],
      }),
      parent,
    });
    return view;
  };

  it("opens a custom panel with a main search field", () => {
    const view = mount("alpha beta alpha");
    expect(findIsOpen(view.state)).toBe(false);
    openFind(view);
    expect(findIsOpen(view.state)).toBe(true);
    const panel = view.dom.querySelector(".cc-find");
    expect(panel).not.toBeNull();
    const input = panel!.querySelector("input[main-field]") as HTMLInputElement;
    expect(input).not.toBeNull();
    view.destroy();
  });

  it("seeds the query from a non-empty selection (VS Code behaviour)", () => {
    // Select "beta" (offsets 6–10).
    const view = mount("alpha beta alpha", { anchor: 6, head: 10 });
    openFind(view);
    const input = view.dom.querySelector(".cc-find input[main-field]") as HTMLInputElement;
    expect(input.value).toBe("beta");
    view.destroy();
  });

  it("clears its query and highlights when closed", () => {
    const view = mount("foo foo");
    openFind(view);
    view.dispatch({ effects: [] }); // no-op
    closeFind(view);
    expect(findIsOpen(view.state)).toBe(false);
    expect(getSearchQuery(view.state).search).toBe("");
    view.destroy();
  });
});
