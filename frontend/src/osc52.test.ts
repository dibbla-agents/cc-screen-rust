import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  decodeOsc52,
  osc52Action,
  Osc52RateLimiter,
  OSC52_CAP,
  previewClipboardText,
  sanitizeClipboardText,
} from "./osc52";

// The payload half of proposal 0077 Part A. Every case below is one of the
// proposal's acceptance criteria: the text arriving here is attacker-shaped by
// construction, so "what exactly happens to this byte" is the contract.

const b64 = (s: string) => Buffer.from(s, "utf8").toString("base64");

describe("decodeOsc52 — selection targets and no-ops (A11)", () => {
  it("accepts the clipboard target", () => {
    expect(decodeOsc52(`c;${b64("hello")}`)?.text).toBe("hello");
  });

  it("accepts the select target and the empty target", () => {
    expect(decodeOsc52(`s;${b64("hi")}`)?.text).toBe("hi");
    expect(decodeOsc52(`;${b64("hi")}`)?.text).toBe("hi");
  });

  it("ignores a p-only write — primary is never promoted to the clipboard", () => {
    expect(decodeOsc52(`p;${b64("secret")}`)).toBeNull();
  });

  it("accepts a combined target that includes c", () => {
    expect(decodeOsc52(`pc;${b64("hi")}`)?.text).toBe("hi");
  });

  it("ignores an unknown target", () => {
    expect(decodeOsc52(`q;${b64("hi")}`)).toBeNull();
    expect(decodeOsc52(`0;${b64("hi")}`)).toBeNull();
  });

  it("treats every shape of the query form as a no-op", () => {
    // The refusal is structural — this module cannot write to the PTY at all
    // (see the source test below). These assertions pin the parse side.
    expect(decodeOsc52("c;?")).toBeNull();
    expect(decodeOsc52(";?")).toBeNull();
    expect(decodeOsc52("p;?")).toBeNull();
    expect(decodeOsc52("cs;?")).toBeNull();
  });

  it("treats empty data as a no-op — we never CLEAR a user's clipboard", () => {
    expect(decodeOsc52("c;")).toBeNull();
    expect(decodeOsc52(";")).toBeNull();
  });

  it("treats malformed base64 as a silent no-op", () => {
    expect(decodeOsc52("c;not!base64")).toBeNull();
    expect(decodeOsc52("c;abc")).toBeNull(); // length % 4 !== 0
    expect(decodeOsc52("no-separator")).toBeNull();
  });

  it("decodes multi-byte UTF-8 rather than mangling it", () => {
    expect(decodeOsc52(`c;${b64("räksmörgås ✓")}`)?.text).toBe("räksmörgås ✓");
  });
});

describe("sanitizeClipboardText (A2) — the attacker-shaped vectors", () => {
  it("drops a trailing newline (the auto-execute vector)", () => {
    expect(sanitizeClipboardText("rm -rf /\n")).toBe("rm -rf /");
  });

  it("drops a bare CR anywhere", () => {
    expect(sanitizeClipboardText("echo hi\rrm -rf /")).toBe("echo hirm -rf /");
  });

  it("normalises CRLF to LF", () => {
    expect(sanitizeClipboardText("a\r\nb")).toBe("a\nb");
  });

  it("strips C0, DEL and C1 controls but keeps tab and newline", () => {
    expect(sanitizeClipboardText("a\x01b\x02c\x7fd\x9be")).toBe("abcde");
    expect(sanitizeClipboardText("a\tb\nc")).toBe("a\tb\nc");
  });

  it("strips bidi overrides and isolates", () => {
    expect(sanitizeClipboardText("safe\u202etxt.exe")).toBe("safetxt.exe");
    expect(sanitizeClipboardText("\u2066a\u2069b")).toBe("ab");
  });

  it("strips trailing whitespace-only lines", () => {
    expect(sanitizeClipboardText("payload\n\n   \n")).toBe("payload");
    expect(sanitizeClipboardText("payload   ")).toBe("payload");
  });

  it("leaves a clean single line alone", () => {
    expect(sanitizeClipboardText("npm run build")).toBe("npm run build");
  });
});

describe("decodeOsc52 — flags feeding the tier split", () => {
  it("marks a payload sanitisation changed", () => {
    const d = decodeOsc52(`c;${b64("rm -rf /\n")}`)!;
    expect(d.text).toBe("rm -rf /");
    expect(d.altered).toBe(true);
  });

  it("does not mark a clean payload as altered", () => {
    expect(decodeOsc52(`c;${b64("clean")}`)!.altered).toBe(false);
  });

  it("marks multi-line payloads", () => {
    const d = decodeOsc52(`c;${b64("one\ntwo")}`)!;
    expect(d.multiline).toBe(true);
    expect(d.altered).toBe(false);
  });

  it("reports the byte length of the decoded text, not the base64", () => {
    expect(decodeOsc52(`c;${b64("räka")}`)!.bytes).toBe(5); // ä is 2 bytes
  });

  it("returns a no-op for a payload that sanitises down to nothing", () => {
    expect(decodeOsc52(`c;${b64("\n\n")}`)).toBeNull();
  });
});

describe("the cap (A8)", () => {
  it("passes a payload just under the cap", () => {
    const d = decodeOsc52(`c;${b64("x".repeat(OSC52_CAP - 1))}`)!;
    expect(d.oversize).toBe(false);
    expect(d.text.length).toBe(OSC52_CAP - 1);
  });

  it("marks a payload over the cap oversize, with no text", () => {
    const d = decodeOsc52(`c;${b64("x".repeat(OSC52_CAP + 100))}`)!;
    expect(d.oversize).toBe(true);
    expect(d.text).toBe("");
    expect(d.bytes).toBeGreaterThan(OSC52_CAP);
  });

  it("an oversize payload is announced, never written or offered", () => {
    const d = decodeOsc52(`c;${b64("x".repeat(OSC52_CAP + 100))}`)!;
    expect(osc52Action(d, true).kind).toBe("announce");
  });

  // Documents the OTHER boundary, which is not ours: above xterm's own
  // PAYLOAD_LIMIT (10M chars) the parser discards the buffer and never calls
  // the handler at all — so the "oversize → toast" behaviour only exists
  // between our cap and that ceiling, and we can never be handed truncated
  // data (a misleading partial copy cannot occur).
  it("documents that xterm's PAYLOAD_LIMIT is above our cap", () => {
    expect(OSC52_CAP).toBeLessThan(10_000_000);
  });
});

describe("the two tiers (A3)", () => {
  const clean = decodeOsc52(`c;${b64("single line")}`)!;
  const multi = decodeOsc52(`c;${b64("one\ntwo")}`)!;
  const dirty = decodeOsc52(`c;${b64("rm -rf /\n")}`)!;

  it("Tier 1: single-line, unaltered, driving → silent write", () => {
    expect(osc52Action(clean, true)).toEqual({ kind: "write", text: "single line", bytes: 11 });
  });

  it("Tier 2: the same payload from a pane the user is NOT driving", () => {
    expect(osc52Action(clean, false).kind).toBe("confirm");
  });

  it("Tier 2: multi-line, even when driving", () => {
    expect(osc52Action(multi, true).kind).toBe("confirm");
  });

  it("Tier 2: sanitisation changed the text, even when driving", () => {
    expect(osc52Action(dirty, true).kind).toBe("confirm");
  });

  it("a no-op decode stays a no-op", () => {
    expect(osc52Action(null, true).kind).toBe("none");
  });
});

describe("previewClipboardText (A6)", () => {
  it("renders newlines and tabs as visible escapes", () => {
    expect(previewClipboardText("a\nb\tc")).toBe("a\\nb\\tc");
  });

  it("renders bidi overrides as visible escapes rather than applying them", () => {
    expect(previewClipboardText("safe\u202etxt")).toBe("safe\\u202etxt");
  });

  it("truncates", () => {
    expect(previewClipboardText("x".repeat(500), 20).length).toBeLessThanOrEqual(21);
  });
});

describe("Osc52RateLimiter (A7)", () => {
  it("allows one sequence per window and drops the rest", () => {
    const l = new Osc52RateLimiter();
    expect(l.allow(0)).toBe("ok");
    expect(l.allow(100)).toBe("dropped");
    expect(l.allow(2999)).toBe("dropped");
    expect(l.allow(3000)).toBe("ok");
  });

  it("disables the session after five drops in the disable window", () => {
    const l = new Osc52RateLimiter();
    expect(l.allow(0)).toBe("ok");
    for (let i = 1; i <= 4; i++) expect(l.allow(i * 10)).toBe("dropped");
    expect(l.allow(50)).toBe("disable");
    expect(l.disabled).toBe(true);
    // Sticky: even a sequence long after the window stays dropped.
    expect(l.allow(1_000_000)).toBe("dropped");
  });

  it("re-enables only on an explicit reset", () => {
    const l = new Osc52RateLimiter();
    l.allow(0);
    for (let i = 1; i <= 5; i++) l.allow(i * 10);
    expect(l.disabled).toBe(true);
    l.reset();
    expect(l.disabled).toBe(false);
    expect(l.allow(0)).toBe("ok");
  });
});

describe("A1 — the structural refusal of the read form", () => {
  // The acceptance criterion is deliberately "the module CANNOT write to the
  // PTY", not "a `?` check fired": a check can be bypassed with `;p;?`, with
  // padding, or with a base64 string that decodes to "?". So this test reads
  // the source and asserts the absence of any path to the socket.
  const raw = readFileSync(join(process.cwd(), "src", "osc52.ts"), "utf8");
  // Comments are prose about the invariant; the invariant itself is the code.
  const src = raw.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "").replace(/^\s*\/\/\/.*$/gm, "");

  it("imports nothing at all", () => {
    expect(/^\s*import\s/m.test(src)).toBe(false);
  });

  it("references no send/socket/terminal write path", () => {
    for (const forbidden of [
      "WebSocket",
      "wsRef",
      ".send(",
      "onData",
      "Terminal",
      "term.input",
      "navigator.clipboard",
      "fetch(",
    ]) {
      expect(src).not.toContain(forbidden);
    }
  });
});
