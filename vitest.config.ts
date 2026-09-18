import { defineConfig } from "vitest/config";

// D-17 首批测试聚焦纯逻辑（parseAppError / SchemaForm 默认值合并 / 剪贴板参数构造）。
// environment 用 jsdom：ipc/client.ts 顶层读 window（IN_TAURI 探测），node 直载会炸。
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
