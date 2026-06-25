import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 5174,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  // M4c/M5 で新規 import した tauri API を起動後 reload で再最適化しないよう事前列挙
  // (WebView2 が reload 中に dom 状態を維持できず exit 0xcfffffff になる回避策)
  optimizeDeps: {
    include: [
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/api/webviewWindow",
    ],
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
    emptyOutDir: true,
  },
});
