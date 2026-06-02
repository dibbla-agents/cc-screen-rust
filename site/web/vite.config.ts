import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// The built site is consumed two ways — GitHub Pages (from /docs on main) and the
// Dibbla `cc-screen` app — so we emit straight into ../../docs and use a relative
// base so the asset URLs work no matter what path the page is served from.
//
// Vite content-hashes every emitted asset (JS, CSS and the imported screenshots),
// which is the cache-busting the old hand-written deploy.sh used to fake with a
// ?v= query: a changed file gets a new filename → a new CDN cache key → it ships
// immediately, while unchanged files keep their long cache.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: "./",
  build: {
    outDir: "../../docs",
    emptyOutDir: true,
  },
});
