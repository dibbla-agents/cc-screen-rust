import { defineConfig } from "vitest/config";

// Standalone vitest config so the PWA/build plugins in vite.config.ts don't run
// during unit tests. The live-preview tests need a DOM (CodeMirror's EditorState
// pulls in browser-ish globals via @codemirror/view types), so use jsdom.
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
