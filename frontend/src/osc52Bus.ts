// The runtime half of OSC 52 clipboard delivery (cc-screen-saas proposal 0077
// Part A): who is allowed to act on a sequence, how often, and how a Tier 2
// payload reaches the app-level toast.
//
// It lives outside the components because every constraint it enforces is
// *cross-component*:
//
//   - A4 the driver gate needs "has this client typed into this session
//     recently", which is knowledge the pane and the editor's agent mirror
//     produce independently;
//   - A5 "exactly one handler acts per sequence" needs an arbiter, because the
//     grid pane and the AgentMirror hold two separate WebSockets onto the SAME
//     session and both parse the same OSC 52;
//   - A10's post-user-copy quiet period is set by App's ⌘C handler and read by
//     every terminal;
//   - the recovery toast is App-level UI fed by a per-pane parser.
//
// Deliberately NOT here: anything that can write to the PTY. The parsing module
// (`osc52.ts`) is import-free by construction; this one only ever touches the
// clipboard sink and React state.

import { Osc52RateLimiter } from "./osc52";

/// Identity of a session across machines — the same key the panes use to tell
/// "claude-x on pine" from "claude-x on mac-studio" apart.
export function sessionKey(session: string, machine?: string): string {
  return `${machine || ""}/${session}`;
}

// ── A5: one acting surface per session ───────────────────────────────────────
//
// Ownership is claimed by whichever eligible surface asks first and released on
// unmount, so the editor's mirror (which mounts over an already-mounted grid
// pane and is the surface the user is looking at) takes over while it is open
// and hands back when it closes.
const owners = new Map<string, object>();

export function claimClipboardSurface(key: string, token: object): void {
  owners.set(key, token);
}

export function releaseClipboardSurface(key: string, token: object): void {
  if (owners.get(key) === token) owners.delete(key);
}

/// True when `token` is the surface that should act on this session's OSC 52.
/// An eligible surface with no incumbent claims ownership lazily, so closing
/// the editor overlay returns the duty to the pane on the next sequence
/// without any teardown ordering to get right.
export function ownsClipboardSurface(key: string, token: object, eligible: boolean): boolean {
  const cur = owners.get(key);
  if (cur === token) return true;
  if (!cur && eligible) {
    owners.set(key, token);
    return true;
  }
  return false;
}

// ── A4: "has this client been driving this session?" ─────────────────────────
//
// Merely being attached is not enough — watching is not driving. A session the
// user has opened but never typed into never qualifies, which is the whole
// point: opening someone else's session must not grant that machine unattended
// write access to your clipboard.
const INPUT_WINDOW_MS = 3 * 60 * 1000;
const lastInput = new Map<string, number>();

/// Record that this client put bytes on this session's stdin. Called from the
/// terminals' onData subscriptions — keystrokes, pastes, and the touch-scroll
/// ladder's mouse reports all count, because all three are the user acting on
/// that session with their own hands.
export function noteSessionInput(key: string): void {
  lastInput.set(key, Date.now());
}

export function drivenRecently(key: string, now = Date.now()): boolean {
  const t = lastInput.get(key);
  return t != null && now - t < INPUT_WINDOW_MS;
}

// ── A10: quiet periods ───────────────────────────────────────────────────────
//
// A copy the user just made must not be swapped out from under them before
// they paste it, so any user-initiated copy suppresses *automatic* writes for a
// while (a Tier 1 payload degrades to a Tier 2 toast — it is not lost).
const USER_COPY_QUIET_MS = 10_000;
let userCopyAt = 0;

export function noteUserCopy(): void {
  userCopyAt = Date.now();
}

export function inUserCopyQuiet(now = Date.now()): boolean {
  return now - userCopyAt < USER_COPY_QUIET_MS;
}

/// How long a fresh attach / Lagged resync suppresses OSC 52 handling entirely,
/// so replayed backlog cannot retroactively write the clipboard and toast as if
/// it had just happened.
export const ATTACH_QUIET_MS = 500;

// ── A7: per-session flood control ────────────────────────────────────────────
const limiters = new Map<string, Osc52RateLimiter>();

export function limiterFor(key: string): Osc52RateLimiter {
  let l = limiters.get(key);
  if (!l) {
    l = new Osc52RateLimiter();
    limiters.set(key, l);
  }
  return l;
}

// ── the offer bus ────────────────────────────────────────────────────────────

/// A staged Tier 2 payload. FROZEN once published: a later OSC 52 creates a new
/// offer and can never mutate a pending one, or the design would be a swap race
/// (show benign, click writes malicious).
export interface ClipboardOffer {
  id: number;
  key: string;
  /// Human label for the source, e.g. `claude-a1b2` or `claude-a1b2 · pine`.
  label: string;
  /// The sanitised text the Copy button will write. Empty for `announceOnly`.
  text: string;
  bytes: number;
  altered: boolean;
  multiline: boolean;
  /// Over the cap: announced, never written, and not offered for copying.
  announceOnly: boolean;
  at: number;
}

/// A session whose OSC 52 handling has been switched off after a flood.
export interface ClipboardDisabled {
  key: string;
  label: string;
}

type OfferListener = (o: ClipboardOffer) => void;
type DisableListener = (d: ClipboardDisabled) => void;
type WroteListener = (w: ClipboardWrote) => void;

/// A Tier 1 write that already happened. Surfaced as a brief confirmation —
/// today a successful assistant copy produces no client-side sign at all, which
/// is both undiscoverable and, for a capability like this one, wrong.
export interface ClipboardWrote {
  label: string;
  bytes: number;
}

const offerListeners = new Set<OfferListener>();
const disableListeners = new Set<DisableListener>();
const wroteListeners = new Set<WroteListener>();
let nextOfferId = 1;

export function subscribeClipboardWrote(fn: WroteListener): () => void {
  wroteListeners.add(fn);
  return () => wroteListeners.delete(fn);
}

export function publishClipboardWrote(w: ClipboardWrote): void {
  wroteListeners.forEach((fn) => fn(w));
}

export function subscribeClipboardOffers(fn: OfferListener): () => void {
  offerListeners.add(fn);
  return () => offerListeners.delete(fn);
}

export function subscribeClipboardDisabled(fn: DisableListener): () => void {
  disableListeners.add(fn);
  return () => disableListeners.delete(fn);
}

export function publishClipboardOffer(
  o: Omit<ClipboardOffer, "id" | "at">
): ClipboardOffer {
  const full: ClipboardOffer = { ...o, id: nextOfferId++, at: Date.now() };
  Object.freeze(full);
  offerListeners.forEach((fn) => fn(full));
  return full;
}

export function publishClipboardDisabled(d: ClipboardDisabled): void {
  disableListeners.forEach((fn) => fn(d));
}

/// A staged offer older than this is not completable — an old click must not
/// complete a write the user has long stopped thinking about.
export const OFFER_TTL_MS = 30_000;

/// Test seam: drop every cross-test remnant of the module-level state.
export function __resetClipboardBus(): void {
  owners.clear();
  lastInput.clear();
  limiters.clear();
  offerListeners.clear();
  disableListeners.clear();
  wroteListeners.clear();
  userCopyAt = 0;
  nextOfferId = 1;
}
