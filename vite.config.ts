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
  },
  envPrefix: ["VITE_", "TAURI_"],
});
