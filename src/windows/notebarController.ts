import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { IN_TAURI } from "../ipc/env";

/**
 * 快速速记条控制器（docs/impl/05 D4）：全局快捷键事件（desktop.note_quick）
 * 在主窗口监听并调用 toggle()。窗口 label=notebar，顶部居中小条。
 */
export async function toggleNoteBar(): Promise<void> {
  if (!IN_TAURI) return;
  const existing = await WebviewWindow.getByLabel("notebar");
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
  const win = new WebviewWindow("notebar", {
    url: "/?w=notebar",
    title: "NexusForge 速记",
    width: 520,
    height: 64,
    decorations: false,
    transparent: true,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: false,
  });
  // 顶部居中
  win.once("tauri://created", async () => {
    try {
      const { PhysicalPosition, currentMonitor } = await import("@tauri-apps/api/window");
      const mon = await currentMonitor();
      if (mon) {
        const x = mon.position.x + Math.floor((mon.size.width - 520) / 2);
        await win.setPosition(new PhysicalPosition(x, mon.position.y + 80));
      }
    } catch {
      /* 定位失败保持默认 */
    }
  });
  win.once("tauri://error", (e) => {
    console.error("速记条窗口创建失败", e);
  });
}
