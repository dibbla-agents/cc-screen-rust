import type { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";

// Which renderer the terminals are actually using. Exposed on `window` beside
// the existing `__ccTerm` debug hook so the smoke suite (and support) can tell
// a WebGL pane from a DOM-fallback one without guessing from pixels.
export type RendererKind = "webgl" | "dom";

function setRenderer(kind: RendererKind) {
  (window as unknown as { __ccRenderer?: RendererKind }).__ccRenderer = kind;
}

// Attach the WebGL renderer to a terminal that has already been `open()`ed,
// falling back to xterm's built-in DOM renderer whenever WebGL2 isn't usable.
//
// Two failure modes, both handled here rather than at the call sites:
//   * no WebGL2 at all (older iOS Safari, VM/GPU-less Linux, headless Chrome
//     without GL flags) — `loadAddon` throws synchronously; we swallow it and
//     stay on the DOM renderer. A terminal must never fail to mount because of
//     the addon.
//   * context loss — iOS Safari frees GPU contexts when a tab backgrounds, and
//     every browser force-loses the oldest context once its cap is hit (layout
//     cycling churns up to 5 contexts here: 4 panes + the editor mirror).
//     Disposing the addon on loss hands rendering back to the DOM renderer, so
//     the standby re-attach path never writes into a permanently blank canvas.
//
// Pinned to @xterm/addon-webgl 0.18.x: 0.19 is built against xterm 6 and
// declares no peer dependency, so it installs silently-broken beside 5.5.
export function attachRenderer(term: Terminal): () => void {
  let addon: WebglAddon | null = null;
  try {
    const a = new WebglAddon();
    a.onContextLoss(() => {
      try {
        a.dispose();
      } catch {
        /* already disposed */
      }
      if (addon === a) addon = null;
      setRenderer("dom");
    });
    term.loadAddon(a);
    addon = a;
    setRenderer("webgl");
  } catch {
    addon = null;
    setRenderer("dom");
  }
  return () => {
    if (!addon) return;
    try {
      addon.dispose();
    } catch {
      /* terminal already disposed */
    }
    addon = null;
  };
}
