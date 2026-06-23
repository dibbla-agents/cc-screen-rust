import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// Dev backend the proxy forwards /api (+ /api/ws) to. Defaults to a local
// server on :8839; set CC_BACKEND=host:port to point at a remote one (e.g. the
// tailnet-bound service) without editing this file.
const backend = process.env.CC_BACKEND ?? "127.0.0.1:8839";

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
        // Pull in the Web Push handlers (push / notificationclick) — a plain-JS
        // static asset, so the generated SW keeps doing precaching as before.
        importScripts: ["push-sw.js"],
        // A new deploy takes over on the next load instead of lingering behind the
        // old precached bundle (the "PWA serves the stale bundle for one session"
        // trap): activate the fresh SW immediately and claim open clients, and
        // drop the previous precache.
        skipWaiting: true,
        clientsClaim: true,
        cleanupOutdatedCaches: true,
      },
    }),
  ],
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api/ws": { target: `ws://${backend}`, ws: true },
      "/api": { target: `http://${backend}` },
    },
  },
});
