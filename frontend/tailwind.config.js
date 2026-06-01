/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // cc-screen status-bar palette (colour236 bg, cyan/amber accents).
        bar: "#0f1720",
        panel: "#161e29",
        edge: "#243042",
        accent: "#38bdf8", // cyan = "input is live"
        amber: "#f5b942", // amber = "settled"
        claude: "#d97757",
        kimi: "#7c8cff",
        gemini: "#4f9bff",
        codex: "#8fd17a",
        shell: "#94a3b8", // slate — deliberately understated vs the AI palette
      },
      fontFamily: {
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
      },
    },
  },
  plugins: [],
};
