import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { IN_TAURI } from "../ipc/env";

/**
 * 快速启动器控制器（docs/impl/05 D1）：全局快捷键事件（desktop.launcher_toggled）
 * 在主窗口监听并调用 toggle()。窗口 label=launcher，已存在则切换显隐。
 */
export async function toggleLauncher(): Promise<void> {
  if (!IN_TAURI) return;
  const existing = await WebviewWindow.getByLabel("launcher");
  if (existing) {
    const visible = await existing.isVisible();
    if (visible) {
      await existing.hide();
    } else {
      await existing.show();
      await existing.setFocus();
    }
    return;
  }
  const win = new WebviewWindow("launcher", {
    url: "/?w=launcher",
    title: "NexusForge 启动器",
    width: 620,
    height: 440,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    skipTaskbar: true,
    center: true,
    shadow: true,
  });
  win.once("tauri://error", (e) => {
    console.error("启动器窗口创建失败", e);
  });
}
