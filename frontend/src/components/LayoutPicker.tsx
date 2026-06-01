import type { Layout } from "./TileGrid";
import { LAYOUT_GLYPHS } from "./LayoutPalette";

interface Props {
  layout: Layout;
  onOpen: () => void;
}

// The header trigger: a single small button showing the active layout's
// glyph. Click (or `Ctrl+B l` / `Ctrl+B Space`) opens the LayoutPalette
// popover, which the parent renders as a sibling so it can anchor below
// the button (top-full / right-0 inside a shared `relative` wrapper).
// Desktop-only — the parent hides this on phones / coarse pointers.
export default function LayoutPicker({ layout, onOpen }: Props) {
  return (
    <button
      onClick={onOpen}
      // The palette uses this data attribute to exempt the trigger from its
      // outside-pointerdown close listener — so clicking the button while
      // the palette is open behaves like a real toggle instead of a
      // close-then-immediately-reopen race. The parent decides which side
      // of the toggle this click runs (`onOpen` is wired to either
      // openPalette or closePalette based on current state).
      data-layout-trigger=""
      title="Pick layout — Ctrl+B then L (or Space)"
      aria-label="Pick layout"
      className="flex h-9 w-9 items-center justify-center rounded-md bg-panel text-slate-300 hover:bg-edge hover:text-slate-100"
    >
      <svg viewBox="0 0 16 16" className="h-5 w-5" aria-hidden>
        <rect x="1" y="1" width="14" height="14" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.4" />
        {LAYOUT_GLYPHS[layout]}
      </svg>
    </button>
  );
}
