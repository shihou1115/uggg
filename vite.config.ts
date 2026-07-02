import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    // Vite 既定の 5173 は他プロジェクトと衝突しやすい (実際に別アプリの dev server と
    // 衝突した) ため ugg 専用ポートに固定。strictPort で「取れなければ即エラー」にし、
    // サイレントに別ポートへ逃げて Tauri (devUrl 固定) が白画面になる罠を防ぐ。
    // tauri.conf.json の build.devUrl と一致させること。
    port: 5273,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 5274,
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
