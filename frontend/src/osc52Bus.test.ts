import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  __resetClipboardBus,
  claimClipboardSurface,
  drivenRecently,
  inUserCopyQuiet,
  noteSessionInput,
  noteUserCopy,
  ownsClipboardSurface,
  publishClipboardOffer,
  releaseClipboardSurface,
  sessionKey,
  subscribeClipboardOffers,
} from "./osc52Bus";

// The runtime gates of proposal 0077 Part A. These are the constraints that
// make the capability safe to have at all, so they get tests of their own
// rather than riding on a browser-level smoke run.

beforeEach(() => __resetClipboardBus());

describe("sessionKey", () => {
  it("distinguishes the same session name on two machines", () => {
    expect(sessionKey("claude-x", "pine")).not.toBe(sessionKey("claude-x", "mac-studio"));
  });
  it("treats a missing machine as the single-agent case", () => {
    expect(sessionKey("claude-x")).toBe(sessionKey("claude-x", ""));
  });
});

describe("A5 — exactly one surface acts per sequence", () => {
  const key = sessionKey("claude-x", "pine");
  const pane = {};
  const mirror = {};

  it("an eligible surface claims an unowned session lazily", () => {
    expect(ownsClipboardSurface(key, pane, true)).toBe(true);
    expect(ownsClipboardSurface(key, pane, true)).toBe(true);
  });

  it("a second surface does NOT act while another owns the session", () => {
    ownsClipboardSurface(key, pane, true);
    // This is the editor-overlay case: the mirror and the grid pane hold two
    // sockets onto the same session, so both parse the same OSC 52. One write,
    // one toast — never two.
    expect(ownsClipboardSurface(key, mirror, true)).toBe(false);
  });

  it("an explicit claim hands ownership over (the mirror opening)", () => {
    ownsClipboardSurface(key, pane, true);
    claimClipboardSurface(key, mirror);
    expect(ownsClipboardSurface(key, mirror, true)).toBe(true);
    expect(ownsClipboardSurface(key, pane, true)).toBe(false);
  });

  it("releasing hands the duty back without any teardown ordering", () => {
    claimClipboardSurface(key, mirror);
    releaseClipboardSurface(key, mirror);
    expect(ownsClipboardSurface(key, pane, true)).toBe(true);
  });

  it("an INELIGIBLE surface never claims an unowned session", () => {
    // A background pane consumes the sequence but performs no action.
    expect(ownsClipboardSurface(key, pane, false)).toBe(false);
    expect(ownsClipboardSurface(key, mirror, true)).toBe(true);
  });

  it("keeps sessions independent", () => {
    const other = sessionKey("claude-y", "pine");
    ownsClipboardSurface(key, pane, true);
    expect(ownsClipboardSurface(other, mirror, true)).toBe(true);
  });
});

describe("A4 — watching is not driving", () => {
  const key = sessionKey("claude-x", "pine");

  it("a session this client never typed into is not driven", () => {
    expect(drivenRecently(key)).toBe(false);
  });

  it("input marks it driven", () => {
    noteSessionInput(key);
    expect(drivenRecently(key)).toBe(true);
  });

  it("the window expires — minutes, not hours", () => {
    noteSessionInput(key);
    expect(drivenRecently(key, Date.now() + 4 * 60 * 1000)).toBe(false);
  });

  it("driving one session says nothing about another", () => {
    noteSessionInput(key);
    expect(drivenRecently(sessionKey("claude-y", "pine"))).toBe(false);
  });
});

describe("A10 — the post-user-copy quiet period", () => {
  it("is off until the user copies", () => {
    expect(inUserCopyQuiet()).toBe(false);
  });
  it("suppresses automatic writes right after a user copy", () => {
    noteUserCopy();
    expect(inUserCopyQuiet()).toBe(true);
    expect(inUserCopyQuiet(Date.now() + 11_000)).toBe(false);
  });
});

describe("A6 — a staged offer is frozen on render", () => {
  it("cannot be mutated after publication", () => {
    const seen: string[] = [];
    subscribeClipboardOffers((o) => seen.push(o.text));
    const offer = publishClipboardOffer({
      key: sessionKey("claude-x", "pine"),
      label: "claude-x",
      text: "benign",
      bytes: 6,
      altered: false,
      multiline: false,
      announceOnly: false,
    });
    expect(Object.isFrozen(offer)).toBe(true);
    // A swap attempt — "show benign, click writes malicious" — cannot land.
    expect(() => {
      (offer as { text: string }).text = "rm -rf /";
    }).toThrow(TypeError);
    expect(offer.text).toBe("benign");
    expect(seen).toEqual(["benign"]);
  });

  it("gives every offer a distinct identity so a later one is a NEW toast", () => {
    const base = {
      key: sessionKey("claude-x", "pine"),
      label: "claude-x",
      bytes: 1,
      altered: false,
      multiline: false,
      announceOnly: false,
    };
    const a = publishClipboardOffer({ ...base, text: "a" });
    const b = publishClipboardOffer({ ...base, text: "b" });
    expect(a.id).not.toBe(b.id);
    expect(a.text).toBe("a");
  });

  it("unsubscribes cleanly", () => {
    const fn = vi.fn();
    const off = subscribeClipboardOffers(fn);
    off();
    publishClipboardOffer({
      key: "k",
      label: "l",
      text: "t",
      bytes: 1,
      altered: false,
      multiline: false,
      announceOnly: false,
    });
    expect(fn).not.toHaveBeenCalled();
  });
});
