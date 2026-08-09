import { useRef, useState } from "react";
import type { SearchAddon } from "@xterm/addon-search";

// Find-bar match highlighting. Colours come from the terminal theme's selection
// family (accent cyan) so a match reads like a selection; the active match is
// the solid accent with a light border. Decorations need `allowProposedApi`,
// which both terminal surfaces already set.
export const FIND_OPTS = {
  decorations: {
    matchBackground: "rgba(56, 189, 248, 0.28)",
    matchBorder: "rgba(56, 189, 248, 0.6)",
    matchOverviewRuler: "#38bdf8",
    activeMatchBackground: "rgba(56, 189, 248, 0.75)",
    activeMatchBorder: "#e0f2fe",
    activeMatchColorOverviewRuler: "#e0f2fe",
  },
};

interface Props {
  // Live SearchAddon of the terminal this bar searches. A ref (not the addon
  // itself) so the bar survives the terminal being rebuilt underneath it.
  search: React.RefObject<SearchAddon | null>;
  onClose: () => void;
}

// The in-pane find bar (proposal 0068 Part E). The WebGL renderer draws the
// terminal as pixels, so the browser's own Cmd/Ctrl+F can no longer find
// terminal output — this replaces it. Cmd/Ctrl+F itself stays untouched
// (proposal 0027's policy); this opens from `Ctrl+B /` or the pane menu.
export default function TerminalFindBar({ search, onClose }: Props) {
  const [q, setQ] = useState("");
  const input = useRef<HTMLInputElement>(null);

  function find(dir: "next" | "prev", term = q) {
    if (!term) return;
    const s = search.current;
    if (!s) return;
    if (dir === "next") s.findNext(term, FIND_OPTS);
    else s.findPrevious(term, FIND_OPTS);
  }

  return (
    <div
      className="absolute right-2 top-1.5 z-20 flex items-center gap-1 rounded border border-edge bg-panel/95 px-1.5 py-1 shadow-lg"
      data-testid="term-find"
    >
      <input
        ref={input}
        // Focus here is always the result of an explicit user gesture (chord or
        // menu item) — the bar only mounts on one — so this doesn't reintroduce
        // the proposal-0009 focus-steal class of bug.
        autoFocus
        placeholder="Find in terminal"
        value={q}
        className="w-40 bg-transparent px-1 py-0.5 text-xs text-slate-100 outline-none placeholder:text-slate-500 sm:w-56"
        onChange={(e) => {
          const v = e.target.value;
          setQ(v);
          // Search as you type; an empty box clears the highlight rather than
          // matching everything.
          if (v) find("next", v);
          else search.current?.clearDecorations();
        }}
        onKeyDown={(e) => {
          // Keep the app's global chords out of the find box.
          e.stopPropagation();
          if (e.key === "Enter") {
            e.preventDefault();
            find(e.shiftKey ? "prev" : "next");
          } else if (e.key === "Escape") {
            e.preventDefault();
            onClose();
          }
        }}
      />
      <button
        className="rounded px-1 text-xs text-slate-400 hover:bg-edge hover:text-slate-100"
        title="Previous match (Shift+Enter)"
        onClick={() => find("prev")}
      >
        ↑
      </button>
      <button
        className="rounded px-1 text-xs text-slate-400 hover:bg-edge hover:text-slate-100"
        title="Next match (Enter)"
        onClick={() => find("next")}
      >
        ↓
      </button>
      <button
        className="rounded px-1 text-xs text-slate-400 hover:bg-edge hover:text-slate-100"
        title="Close (Esc)"
        onClick={onClose}
      >
        ✕
      </button>
    </div>
  );
}
