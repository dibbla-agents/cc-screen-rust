import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// Dev: `npm run dev` serves on :5173 and proxies /api (and the /api/ws
// WebSocket) to the Rust server on :8839, so you run both during development.
// Prod: `npm run build` emits into ./dist, which the Rust binary embeds via
// rust-embed — one static binary, no separate web server.
export default defineConfig({
  plugins: [
    react(),
    VitePWA({
      registerType: "autoUpdate",
      includeAssets: ["apple-touch-icon.png", "favicon.png"],
      manifest: {
        name: "cc-screen",
        short_name: "cc-screen",
        description: "Drive cc-screen tmux agents from your phone",
        theme_color: "#0f1720",
        background_color: "#0f1720",
        display: "standalone",
        orientation: "any",
        icons: [
          { src: "icon-192.png", sizes: "192x192", type: "image/png" },
          { src: "icon-512.png", sizes: "512x512", type: "image/png" },
          {
            src: "icon-512.png",
            sizes: "512x512",
            type: "image/png",
            purpose: "maskable",
          },
        ],
      },
      workbox: {
        // Never cache the API; the terminal must always hit the live server.
        navigateFallbackDenylist: [/^\/api/],
      },
    }),
  ],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api/ws": { target: "ws://127.0.0.1:8839", ws: true },
      "/api": { target: "http://127.0.0.1:8839" },
    },
  },
});
