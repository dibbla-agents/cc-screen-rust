// Screenshot imports. Importing them (rather than referencing /img/*.png) lets
// Vite content-hash each file into docs/assets/, so a re-exported screenshot
// busts the CDN cache automatically instead of going stale for hours.
//
// Each entry carries its intrinsic pixel size; we feed those to width/height on
// the <img> so the browser reserves the right box before the image loads (no
// layout shift) and every device frame stays aligned.
//
// The redesign (0057 Tour) uses five shots. mobile-editor / web-new-session /
// tui-layouts stay on disk for docs use but are no longer imported here, so
// Vite doesn't bundle them into the page.
import mobileSessions from "./assets/img/mobile-sessions.png";
import mobileAgent from "./assets/img/mobile-agent.png";
import mobileFiles from "./assets/img/mobile-files.png";
import webCowork from "./assets/img/web-cowork.png";
import tuiGrid from "./assets/img/tui-grid.png";

export type Shot = {
  src: string;
  w: number;
  h: number;
  alt: string;
};

export const shots = {
  mobileSessions: {
    src: mobileSessions,
    w: 920,
    h: 1565,
    alt: "The cc-screen session list on a phone — Claude, Codex and shell agents, each with its status and last-active time.",
  },
  mobileAgent: {
    src: mobileAgent,
    w: 920,
    h: 1565,
    alt: "A live Claude session on a phone, with a tap-to-send key bar: Ctrl-C, Esc, Tab, arrows and Enter.",
  },
  mobileFiles: {
    src: mobileFiles,
    w: 920,
    h: 1565,
    alt: "The file tree of a project, browsed from a phone, with per-file download buttons.",
  },
  webCowork: {
    src: webCowork,
    w: 1700,
    h: 999,
    alt: "The browser app showing a file tree, a rendered Markdown file, and the live agent terminal side by side.",
  },
  tuiGrid: {
    src: tuiGrid,
    w: 1700,
    h: 1064,
    alt: "The ccs terminal client with four agent panes in a quad grid and a box menu open.",
  },
} satisfies Record<string, Shot>;
