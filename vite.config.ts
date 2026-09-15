import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// NexusForge 前端构建配置（docs/UI-PLAN.md U1-1）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Tauri WebView2（Chromium 内核）目标
    target: "chrome105",
    outDir: "dist",
    emptyOutDir: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
});
