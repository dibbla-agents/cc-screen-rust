import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InlineName } from "./TileGrid";

// Proposal 0081 Part H — the identity bar's inline rename field ([0035]) is
// opened by a *counter bump* from the ⌃B r chord. The bump used to be delivered
// by swapping the prop per pane (`idx === active ? seq : -1`), which meant that
// changing the active pane changed the prop on BOTH the pane you left and the
// pane you landed on — so every ⌃B ←/→ opened a rename box, the focused <input>
// then swallowed the whole ⌃B prefix, and blur-commit POSTed a display label
// nobody asked for.
//
// The fix is the pattern TerminalView's searchSignal already uses: broadcast the
// counter unchanged and filter on `active` in the receiver, consuming the bump
// *before* the active test so an inactive pane can never replay it later.
//
// These are the repo's first tests that render a piece of TileGrid. No
// testing-library — createRoot + act under jsdom is enough for an effect
// contract, and adds no dependency.

declare global {
  // eslint-disable-next-line no-var
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  globalThis.IS_REACT_ACT_ENVIRONMENT = false;
});

const field = () => container.querySelector("input");

function render(props: {
  editSeq: number;
  paneActive: boolean;
  value?: string;
  onCommit?: (label: string | null) => void;
}) {
  act(() => {
    root.render(
      <InlineName
        value={props.value ?? "claude-api"}
        short="claude-api"
        nameColor="text-slate-100"
        editSeq={props.editSeq}
        paneActive={props.paneActive}
        onCommit={props.onCommit ?? (() => {})}
      />
    );
  });
}

describe("InlineName — the ⌃B r seq contract (0081 Part H)", () => {
  it("does not open on mount, however large the seq", () => {
    render({ editSeq: 7, paneActive: true });
    expect(field()).toBeNull();
  });

  it("does not open when the seq is unchanged and the pane is re-rendered", () => {
    render({ editSeq: 3, paneActive: true });
    render({ editSeq: 3, paneActive: true });
    expect(field()).toBeNull();
  });

  it("opens on a bump in the active pane, with the name selected", () => {
    render({ editSeq: 3, paneActive: true });
    render({ editSeq: 4, paneActive: true });
    const el = field()!;
    expect(el).not.toBeNull();
    expect(el.value).toBe("claude-api");
    expect(document.activeElement).toBe(el);
    expect(el.selectionStart).toBe(0);
    expect(el.selectionEnd).toBe("claude-api".length);
  });

  it("ignores a bump in an inactive pane, and does not replay it later", () => {
    render({ editSeq: 3, paneActive: false });
    render({ editSeq: 4, paneActive: false }); // someone pressed ⌃B r elsewhere
    expect(field()).toBeNull();
    // Now this pane becomes the focused one — the classic replay hazard. The
    // bump was consumed while inactive, so nothing may open.
    render({ editSeq: 4, paneActive: true });
    expect(field()).toBeNull();
  });

  it("does not open merely because the pane became active", () => {
    // This is the bug in one line: focus moving between panes is not a bump.
    render({ editSeq: 2, paneActive: false });
    render({ editSeq: 2, paneActive: true });
    expect(field()).toBeNull();
  });

  it("commits nothing when the field is dismissed unchanged", () => {
    const onCommit = vi.fn();
    render({ editSeq: 1, paneActive: true, onCommit });
    render({ editSeq: 2, paneActive: true, onCommit });
    const el = field()!;
    act(() => {
      // React delegates onBlur through the bubbling `focusout` event.
      el.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(onCommit).not.toHaveBeenCalled();
  });

  it("commits a real edit exactly once", () => {
    const onCommit = vi.fn();
    render({ editSeq: 1, paneActive: true, onCommit });
    render({ editSeq: 2, paneActive: true, onCommit });
    const el = field()!;
    act(() => {
      // React tracks the DOM value node, so set it through the native setter.
      const setter = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value"
      )!.set!;
      setter.call(el, "billing rewrite");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    act(() => {
      // React delegates onBlur through the bubbling `focusout` event.
      el.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith("billing rewrite");
  });

  it("clearing the field still clears the label (commits null)", () => {
    const onCommit = vi.fn();
    render({ editSeq: 1, paneActive: true, value: "My label", onCommit });
    render({ editSeq: 2, paneActive: true, value: "My label", onCommit });
    const el = field()!;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value"
      )!.set!;
      setter.call(el, "   ");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    act(() => {
      // React delegates onBlur through the bubbling `focusout` event.
      el.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    expect(onCommit).toHaveBeenCalledTimes(1);
    expect(onCommit).toHaveBeenCalledWith(null);
  });
});
