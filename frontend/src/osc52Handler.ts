// The OSC 52 handler both terminal surfaces register (cc-screen-saas proposal
// 0077 Part A). Shared so the grid pane and the editor's agent mirror enforce
// one set of rules — a constraint that held in one surface and not the other
// would be no constraint at all.

import { decodeOsc52, osc52Action } from "./osc52";
import {
  drivenRecently,
  inUserCopyQuiet,
  limiterFor,
  ownsClipboardSurface,
  publishClipboardDisabled,
  publishClipboardOffer,
} from "./osc52Bus";
import { writeClipboardUnattended } from "./util";

export interface Osc52Ctx {
  /// `sessionKey(session, machine)` — the identity every gate is keyed by.
  /// A getter, not a value: a surface outlives a machine change.
  key: () => string;
  /// What the toast names as the source ("claude-a1b2", "claude-a1b2 · pine").
  label: () => string;
  /// This surface's identity for the one-acting-surface arbiter (A5).
  token: object;
  /// A5: is this surface a candidate to act for the session at all? (The active
  /// grid pane, or the editor's mirror while the overlay is open.)
  eligible: () => boolean;
  /// A4: does this terminal hold DOM focus AND is the document focused?
  focused: () => boolean;
  /// A10: timestamp before which handling is suppressed entirely (attach and
  /// Lagged-resync backlog).
  quietUntil: () => number;
  /// Feedback for a successful silent write — also what makes the feature
  /// discoverable, since a successful assistant copy has no other client-side
  /// sign.
  onWrote: (bytes: number) => void;
}

/// Build the `registerOscHandler(52, …)` callback.
///
/// A9: the returned function is synchronous and returns `true` — never a
/// Promise. A pending promise pauses xterm's OSC parser and stalls ALL output
/// for that terminal until it resolves, which a hostile stream plus a slow
/// clipboard sink turns into head-of-line blocking. The clipboard write is
/// fire-and-forget with a `.catch`.
export function makeOsc52Handler(ctx: Osc52Ctx): (payload: string) => boolean {
  return (payload: string) => {
    try {
      handle(payload, ctx);
    } catch {
      /* a malformed payload must never break the terminal's parser */
    }
    return true; // consumed either way — never echoed, never answered
  };
}

function handle(payload: string, ctx: Osc52Ctx): void {
  const now = Date.now();
  // A10: replayed backlog after attach / resync writes nothing.
  if (now < ctx.quietUntil()) return;
  // A5: background panes and the non-active mirror parse and consume the
  // sequence but perform no clipboard action.
  const eligible = ctx.eligible();
  if (!ownsClipboardSurface(ctx.key(), ctx.token, eligible)) return;

  const lim = limiterFor(ctx.key());
  if (lim.disabled) return;

  // Decode BEFORE spending the rate-limit budget: a malformed or empty
  // sequence is a no-op, not an event worth throttling.
  const d = decodeOsc52(payload);
  if (!d) return;

  const verdict = lim.allow(now);
  if (verdict === "disable") {
    publishClipboardDisabled({ key: ctx.key(), label: ctx.label() });
    return;
  }
  if (verdict === "dropped") return;

  // A4 + A10: the silent tier needs the pane the user is actively driving, with
  // focus, and outside the window in which the user's own copy is still fresh.
  const driving = eligible && ctx.focused() && drivenRecently(ctx.key(), now) && !inUserCopyQuiet(now);

  const act = osc52Action(d, driving);
  switch (act.kind) {
    case "none":
      return;
    case "write":
      // Unattended writes take the async Clipboard API ONLY. writeClipboard's
      // execCommand fallback focuses and selects a hidden textarea, destroying
      // any live document selection and stealing focus — acceptable inside a
      // user gesture, never behind the user's back. Where it is unavailable we
      // degrade to the toast, whose click IS a gesture.
      writeClipboardUnattended(act.text)
        .then(() => ctx.onWrote(act.bytes))
        .catch(() =>
          publishClipboardOffer({
            key: ctx.key(),
            label: ctx.label(),
            text: act.text,
            bytes: act.bytes,
            altered: false,
            multiline: false,
            announceOnly: false,
          })
        );
      return;
    case "confirm":
      publishClipboardOffer({
        key: ctx.key(),
        label: ctx.label(),
        text: act.text,
        bytes: act.bytes,
        altered: act.altered,
        multiline: act.multiline,
        announceOnly: false,
      });
      return;
    case "announce":
      publishClipboardOffer({
        key: ctx.key(),
        label: ctx.label(),
        text: "",
        bytes: act.bytes,
        altered: false,
        multiline: false,
        announceOnly: true,
      });
      return;
  }
}
