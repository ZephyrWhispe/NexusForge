import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { IN_TAURI } from "../ipc/env";

/**
 * 剪切板快速面板控制器（docs/UI-PLAN.md U3-5）。
 * 全局快捷键事件（clipboard.quick_panel_toggled）在主窗口监听并调用 toggle()。
 * 面板窗口：label=quickpanel，无框置顶小窗；已存在则切换显隐。
 */
export async function toggleQuickPanel(): Promise<void> {
  if (!IN_TAURI) return;
  const existing = await WebviewWindow.getByLabel("quickpanel");
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
  const win = new WebviewWindow("quickpanel", {
    url: "/?w=quickpanel",
    title: "NexusForge 快速面板",
    width: 560,
    height: 420,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    skipTaskbar: true,
    center: true,
    shadow: true,
  });
  win.once("tauri://error", (e) => {
    console.error("快速面板创建失败", e);
  });
}
