// OSC 52 — the payload side of "a copy performed inside a session reaches the
// clipboard of the machine the user is sitting at" (cc-screen-saas proposal
// 0077 Part A).
//
// STRUCTURAL INVARIANT (0077 A1): this module has no way to write to the PTY.
// It imports nothing, holds no socket, no `Terminal`, no send function, and
// every export is a pure function over strings. That is what makes the OSC 52
// *query* form (`ESC]52;c;?`, which a terminal would answer by base64-ing the
// user's clipboard back up the wire) unanswerable BY CONSTRUCTION rather than
// by a check that could be missed with `;p;?`, padding, or a base64 string that
// decodes to "?". `src/osc52.test.ts` pins that property by reading this file.
//
// The text arriving here is attacker-influenceable by construction — the
// assistant runs with --dangerously-skip-permissions, so a prompt injection in
// a fetched page or a diff yields arbitrary bytes on the PTY, hence arbitrary
// OSC 52 — which is why sanitisation and tiering live here and are not
// optional. The runtime gates (who is driving, rate limiting, the toast) live
// in osc52Bus.ts and the terminal components.

/// Largest decoded payload we will act on. Far above any legitimate /copy, far
/// below anything that hurts. Note this bounds what is *written*, not what is
/// allocated: registering an OSC handler makes xterm accumulate the payload up
/// to its own PAYLOAD_LIMIT (10M chars) before our code runs — the rate limiter
/// is what bounds memory. Above that ceiling xterm discards the buffer and
/// never invokes the callback, so we can never be handed truncated data.
export const OSC52_CAP = 64 * 1024;

/// A decoded, sanitised OSC 52 write.
export interface Osc52Decoded {
  /// The text after sanitizeClipboardText().
  text: string;
  /// Byte length (UTF-8) of the decoded text, before sanitisation — what the
  /// toast reports, because it is what the session actually sent.
  bytes: number;
  /// True when sanitisation changed the text. Any change disqualifies the
  /// silent tier: the user must see what they are about to paste.
  altered: boolean;
  /// True when the sanitised text spans more than one line.
  multiline: boolean;
  /// True when the payload exceeded OSC52_CAP (then `text` is empty).
  oversize: boolean;
}

/// What the client should do with a sequence.
export type Osc52Action =
  /// Nothing at all — a query, a p-only write, empty data, malformed base64, an
  /// unknown selection target. The sequence is consumed; no user-visible trace.
  | { kind: "none" }
  /// Tier 1: write the clipboard silently (single-line, unaltered, under the
  /// cap, and from the pane the user is driving).
  | { kind: "write"; text: string; bytes: number }
  /// Tier 2: never automatic — offer it as a click-to-copy toast.
  | { kind: "confirm"; text: string; bytes: number; altered: boolean; multiline: boolean }
  /// Over the cap: announced, not written, and not offered for copying either.
  | { kind: "announce"; bytes: number };

// C0 controls except \t and \n, plus DEL and the C1 block. The auto-execute
// vector is a trailing "\n" or a bare "\r" pasted into a shell without
// bracketed paste — ~40 bytes long, so the size cap does nothing about it.
const CONTROLS = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g;
// Bidi overrides/isolates and the other bidi format characters. Stripped rather
// than merely displayed: they defeat a naive preview, which is exactly what the
// recovery toast shows the user before they click Copy.
const BIDI = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

/// Normalise clipboard text arriving from a session (0077 A2).
///
/// - CRLF → LF, bare CR dropped (a bare CR is a shell "execute this now").
/// - All other C0/C1 controls and DEL stripped; \t and \n survive.
/// - Bidi overrides/isolates stripped.
/// - Trailing newlines and trailing whitespace-only lines stripped.
export function sanitizeClipboardText(s: string): string {
  let out = s.replace(/\r\n/g, "\n").replace(/\r/g, "");
  out = out.replace(CONTROLS, "").replace(BIDI, "");
  // Trailing whitespace-only lines (including the final newline itself).
  out = out.replace(/(?:[ \t]*\n)+[ \t]*$/, "").replace(/[ \t]+$/, "");
  return out;
}

/// UTF-8 byte length of a JS string, without allocating an encoder per call.
const enc = typeof TextEncoder !== "undefined" ? new TextEncoder() : null;
function utf8Len(s: string): number {
  if (enc) return enc.encode(s).length;
  return unescape(encodeURIComponent(s)).length; // ancient-fallback, never hit
}

/// Base64 → UTF-8 string, or null when the payload isn't valid base64.
function fromBase64(b64: string): string | null {
  const compact = b64.replace(/\s+/g, "");
  if (!compact) return null;
  if (compact.length % 4 !== 0) return null;
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(compact)) return null;
  let bin: string;
  try {
    bin = atob(compact);
  } catch {
    return null;
  }
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  try {
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    return bin;
  }
}

/// Decode one OSC 52 payload — the string xterm hands the handler, i.e.
/// everything after `ESC ] 52 ;`, UNSPLIT (`ESC]52;c;?` arrives as `"c;?"`).
///
/// Returns null for every no-op case (0077 A11): the `?` query form, empty
/// data (which means "clear the clipboard" in the de-facto spec — cc-screen
/// never clears a user's clipboard on remote instruction), malformed base64, a
/// missing separator, and a write that targets only the primary selection
/// (`p`), which must never be promoted to the system clipboard.
export function decodeOsc52(payload: string): Osc52Decoded | null {
  const sep = payload.indexOf(";");
  if (sep < 0) return null;
  const targets = payload.slice(0, sep);
  const data = payload.slice(sep + 1);
  // Accept the system clipboard (`c`), the "select" buffer (`s`) and the empty
  // target (which means `s0` → clipboard by convention). Anything else — `p`,
  // the cut-buffers `0`-`7`, junk — is not ours to write.
  if (targets && !/[cs]/.test(targets)) return null;
  if (!data) return null;
  if (data === "?") return null; // query: never answered; see A1

  // Cheap pre-check so a hostile 9 MB payload isn't decoded just to be dropped.
  const approx = Math.floor((data.length * 3) / 4);
  if (approx > OSC52_CAP) return { text: "", bytes: approx, altered: false, multiline: false, oversize: true };

  const raw = fromBase64(data);
  if (raw === null) return null;
  const bytes = utf8Len(raw);
  if (bytes > OSC52_CAP) return { text: "", bytes, altered: false, multiline: false, oversize: true };

  const text = sanitizeClipboardText(raw);
  if (!text) return null; // whitespace-only / control-only: nothing to copy
  return { text, bytes, altered: text !== raw, multiline: text.includes("\n"), oversize: false };
}

/// The two-tier split (0077 A3). `driving` is the caller's verdict on the
/// driver gate (A4) *and* the post-user-copy quiet period (A10) — this function
/// deliberately does not know how either is computed.
export function osc52Action(d: Osc52Decoded | null, driving: boolean): Osc52Action {
  if (!d) return { kind: "none" };
  if (d.oversize) return { kind: "announce", bytes: d.bytes };
  if (driving && !d.multiline && !d.altered) return { kind: "write", text: d.text, bytes: d.bytes };
  return { kind: "confirm", text: d.text, bytes: d.bytes, altered: d.altered, multiline: d.multiline };
}

/// Render text for the recovery toast: truncated, single-line, with control and
/// bidi characters shown as visible escapes. The toast tells the user what they
/// are about to put on their clipboard, so it must not be fooled by the very
/// characters A2 strips (a sanitised payload has none left, but an *unsanitised*
/// preview of the raw text would be).
export function previewClipboardText(text: string, max = 120): string {
  let out = "";
  for (const ch of text) {
    const c = ch.codePointAt(0)!;
    const invisible =
      c < 0x20 ||
      c === 0x7f ||
      (c >= 0x80 && c <= 0x9f) ||
      /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/.test(ch);
    if (ch === "\n") out += "\\n";
    else if (ch === "\t") out += "\\t";
    else if (invisible) out += `\\u${c.toString(16).padStart(4, "0")}`;
    else out += ch;
    if (out.length >= max) return out.slice(0, max) + "…";
  }
  return out;
}

/// Per-session flood control (0077 A7).
///
/// One actionable sequence per WINDOW; excess is dropped and never queued.
/// After DISABLE_AFTER drops inside DISABLE_WINDOW the session's OSC 52
/// handling switches off behind a sticky banner — an assistant copying five
/// times in half a minute is not doing what the user asked. This also protects
/// the app's single toast slot from being used as a UI-suppression channel.
export class Osc52RateLimiter {
  static readonly WINDOW_MS = 3000;
  static readonly DISABLE_AFTER = 5;
  static readonly DISABLE_WINDOW_MS = 30000;

  // -Infinity, not 0: the FIRST sequence must always pass, and a test that
  // drives the limiter from t=0 would otherwise see it dropped.
  private last = Number.NEGATIVE_INFINITY;
  private drops: number[] = [];
  /// Sticky: only an explicit user action clears it.
  disabled = false;

  /// "ok" → act on this sequence. "dropped" → silently ignore it.
  /// "disable" → ignore it AND raise the banner (returned exactly once).
  allow(now: number): "ok" | "dropped" | "disable" {
    if (this.disabled) return "dropped";
    if (now - this.last >= Osc52RateLimiter.WINDOW_MS) {
      this.last = now;
      return "ok";
    }
    this.drops = this.drops.filter((t) => now - t < Osc52RateLimiter.DISABLE_WINDOW_MS);
    this.drops.push(now);
    if (this.drops.length >= Osc52RateLimiter.DISABLE_AFTER) {
      this.disabled = true;
      this.drops = [];
      return "disable";
    }
    return "dropped";
  }

  /// Re-enable after the user dismisses the banner. Deliberately explicit —
  /// nothing in the byte stream can undo a disable.
  reset(): void {
    this.disabled = false;
    this.drops = [];
    this.last = Number.NEGATIVE_INFINITY;
  }
}
