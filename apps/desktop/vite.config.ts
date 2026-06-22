import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// Tauri-aware Vite config: fixed dev port, no clearScreen so we see Cargo logs.
export default defineConfig({
  plugins: [solid()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      // Don't crawl src-tauri (Rust side has its own watcher).
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: true,
    rollupOptions: {
      // Two entry points: the main app and the standalone overlay HUD window.
      input: {
        main: "index.html",
        overlay: "overlay.html",
      },
    },
  },
});
