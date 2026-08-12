// Is the ⌃B prefix currently armed?
//
// A module-level flag, written only by App's keydown engine and read by the
// other window-capture listeners that would otherwise fight it. It exists
// because of one collision, and it is worth stating precisely (proposal 0081
// Part C):
//
// The empty grid pane *is* the session switcher ([0026]), and while it is the
// focused pane it registers a window **capture** keydown that consumes bare
// ↑/↓/⏎/Esc to drive its own cursor. App's prefix engine registers a capture
// keydown too. React runs child effects before parent effects, so the
// switcher's listener is registered first and therefore fires first — and it
// calls stopPropagation(), not stopImmediatePropagation(), so App's handler
// runs anyway. Before Part C that was invisible (focus sat in the switcher's
// filter, so App bailed out of every key); now that the prefix works over an
// empty pane, an armed `⌃B ↑` would move the switcher's cursor *and* cycle the
// pane's session — one keypress, two actions.
//
// Rather than depend on listener registration order, the switcher asks. Only
// the *armed* window is gated: the post-arrow repeat window ([0011]) still
// belongs to the switcher on an empty pane, which is deliberate — an empty pane
// has no session to cycle, so bare arrows there have nothing else to do.
let armed = false;

export function setPrefixArmed(v: boolean): void {
  armed = v;
}

// True between a ⌃B keydown and its chord key (or the 600 ms timeout).
export function prefixArmed(): boolean {
  return armed;
}
