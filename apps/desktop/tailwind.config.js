/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      fontFamily: {
        ui: ["var(--font-ui)"],
        mono: [
          "JetBrains Mono",
          "IBM Plex Mono",
          "SF Mono",
          "Menlo",
          "Consolas",
          "ui-monospace",
          "monospace",
        ],
      },
      colors: {
        bg: "var(--c-bg)",
        surface: "var(--c-surface)",
        surface2: "var(--c-surface2)",
        hover: "var(--c-hover)",
        line: "var(--c-line)",
        line2: "var(--c-line2)",
        text: "var(--c-text)",
        text2: "var(--c-text2)",
        muted: "var(--c-muted)",
        faint: "var(--c-faint)",
        accent: "var(--c-accent)",
        "accent-bg": "var(--c-accent-bg)",
        danger: "var(--c-danger)",
        warn: "var(--c-warn)",
        ok: "var(--c-ok)",
        // legacy aliases for any stragglers
        fg: "var(--c-text)",
        err: "var(--c-danger)",
      },
    },
  },
  plugins: [],
};
