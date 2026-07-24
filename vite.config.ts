import { resolve } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev server port and strict mode.
export default defineConfig(async () => ({
  // Vite root is `src/` so `src/index.html` is the entry, with the
  // TypeScript source co-located. tauri.conf.json's `frontendDist`
  // is `../dist` (relative to src-tauri), so Vite's `outDir: "../dist"`
  // emits to the right place.
  root: "src",
  publicDir: "src/public",
  plugins: [react()],
  resolve: {
    alias: {
      "@shared": resolve(__dirname, "src/shared"),
      "@renderer": resolve(__dirname, "src"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    hmr: {
      protocol: "ws",
      host: "127.0.0.1",
      port: 1421,
    },
    watch: {
      // Don't watch the Rust source tree (Tauri handles that itself).
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: ["es2021", "chrome105", "safari13"],
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    outDir: "../dist",
    emptyOutDir: true,
  },
}));