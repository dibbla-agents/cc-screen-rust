import { useEffect, useRef, useState } from "react";

// True while `open`, and for `ms` after it flips false.
//
// Overlays that slide/fade (the session drawer, the status view) must keep
// their root mounted so CSS can animate the transition out — but they used to
// keep their *contents* mounted too, so every session row (and the status dot
// on it) stayed live and rendering behind a closed overlay. Gating the body on
// this hook unmounts the rows once the transition has finished, without losing
// the animation in either direction (proposal 0068 Part B).
export function useOpenOrClosing(open: boolean, ms: number): boolean {
  const [closing, setClosing] = useState(false);
  const prev = useRef(open);
  useEffect(() => {
    const wasOpen = prev.current;
    prev.current = open;
    if (!wasOpen || open) return;
    setClosing(true);
    const id = setTimeout(() => setClosing(false), ms);
    return () => clearTimeout(id);
  }, [open, ms]);
  return open || closing;
}
