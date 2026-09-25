import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// NexusForge 前端构建配置（docs/UI-PLAN.md U1-1）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    // Rust 编译产物不参与 HMR watch：cargo build 期间的 dll 文件锁（EBUSY）会杀死 dev server
    watch: {
      ignored: ["**/target/**"],
    },
  },
  build: {
    // Tauri WebView2（Chromium 内核）目标
    target: "chrome105",
    outDir: "dist",
    emptyOutDir: true,
    // PERF-03：大依赖分包——monaco/xterm/fluentui 不随业务 chunk 重下载，
    // 路由级 lazy() 边界 + 独立 chunk 让缓存命中粒度更细
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          if (id.includes("monaco-editor")) return "monaco";
          if (id.includes("@xterm")) return "xterm";
          if (id.includes("@fluentui")) return "fluentui";
          if (id.includes("node_modules")) return "vendor";
        },
      },
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
});
