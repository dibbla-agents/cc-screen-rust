import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { HIDDEN_SESSIONS_MS, usePoll } from "./poll";

// A minimal hook harness — the repo has no component-test library, and this
// needs only "mount a component that calls the hook".
(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
function renderHook(use: () => void): { unmount: () => void } {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  const Probe = () => {
    use();
    return null;
  };
  act(() => {
    root.render(createElement(Probe));
  });
  return {
    unmount: () =>
      act(() => {
        root.unmount();
      }),
  };
}

// Drive document.visibilityState + the events the hook listens to.
function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => state,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("usePoll (proposal 0068 Part C)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setVisibility("visible");
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("keeps the visible cadence unchanged", () => {
    const fn = vi.fn();
    renderHook(() => usePoll(fn, 4000));
    vi.advanceTimersByTime(12_000);
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("pauses entirely while hidden and refetches once on return", () => {
    const fn = vi.fn();
    renderHook(() => usePoll(fn, 4000));
    setVisibility("hidden");
    vi.advanceTimersByTime(60_000);
    expect(fn).toHaveBeenCalledTimes(0);
    setVisibility("visible");
    expect(fn).toHaveBeenCalledTimes(1); // the quiet refetch on return
  });

  it("keeps a slow heartbeat while hidden when one is asked for", () => {
    const fn = vi.fn();
    renderHook(() => usePoll(fn, 4000, { hiddenMs: HIDDEN_SESSIONS_MS }));
    setVisibility("hidden");
    vi.advanceTimersByTime(180_000);
    // 3 minutes hidden → 3 heartbeats, not 45 polls.
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("dedupes focus against the visibility refetch", () => {
    const fn = vi.fn();
    renderHook(() => usePoll(fn, 4000, { onFocus: true }));
    setVisibility("hidden");
    setVisibility("visible");
    window.dispatchEvent(new Event("focus"));
    expect(fn).toHaveBeenCalledTimes(1); // one fetch on return, not two
  });

  it("does not run at all while disabled", () => {
    const fn = vi.fn();
    renderHook(() => usePoll(fn, 1000, { enabled: false, immediate: true }));
    vi.advanceTimersByTime(10_000);
    expect(fn).not.toHaveBeenCalled();
  });
});
