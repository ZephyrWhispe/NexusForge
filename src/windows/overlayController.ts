import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import { IN_TAURI } from "../ipc/env";
import { screenshotStart, screenshotDiscard, type TaskStartDto } from "../ipc/client";

/**
 * 截图覆盖层控制器（docs/impl/03 P3，docs/UI-PLAN.md U5）。
 *
 * 流程：screenshot_start（Rust 抓全屏帧）→ 创建覆盖层窗口（虚拟桌面物理坐标
 * 显式定位，规避 docs/impl/03 P3 fullscreen:true 多屏陷阱）→ 窗口加载后取帧。
 * 单任务模型：已有覆盖层时直接聚焦（新任务会替换旧帧）。
 */
export async function startOverlay(mode: "shot" | "ocr"): Promise<void> {
  if (!IN_TAURI) return;
  const existing = await WebviewWindow.getByLabel("overlay");
  if (existing) {
    await existing.show();
    await existing.setFocus();
    return;
  }
  let info: TaskStartDto;
  try {
    info = await screenshotStart(mode);
  } catch (e) {
    console.error("截图启动失败", e);
    return;
  }
  const win = new WebviewWindow("overlay", {
    url: `/?w=overlay&task=${info.task_id}`,
    title: "NexusForge 截图",
    decorations: false,
    transparent: false,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: false,
    resizable: false,
    visible: false, // 定位完成后再显示，避免闪跳
  });
  win.once("tauri://error", (e) => {
    console.error("覆盖层创建失败", e);
  });
  win.once("tauri://created", async () => {
    try {
      // 物理像素显式定位（多显示器/高 DPI 下与抓帧坐标系一致）
      await win.setPosition(new PhysicalPosition(info.x, info.y));
      await win.setSize(new PhysicalSize(info.width, info.height));
      await win.show();
      await win.setFocus();
    } catch (e) {
      console.error("覆盖层定位失败", e);
    }
  });
}

/** 取消任务：丢弃帧 + 关闭覆盖层（覆盖层内部 Esc 也会走此路径） */
export async function cancelOverlay(taskId: string | null): Promise<void> {
  if (taskId) await screenshotDiscard(taskId).catch(() => undefined);
  const overlay = await WebviewWindow.getByLabel("overlay");
  await overlay?.close();
}

/** Pin 贴图窗口 label（唯一性） */
function pinLabel(id: string): string {
  return `pin_${id.replace(/-/g, "")}`;
}

/** 创建/聚焦贴图窗口（docs/impl/03 P6） */
export async function openPinWindow(pin: {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  zoom: number;
  opacity: number;
}): Promise<void> {
  if (!IN_TAURI) return;
  const label = pinLabel(pin.id);
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    await existing.show();
    return;
  }
  const w = Math.max(40, Math.round(pin.width * pin.zoom));
  const h = Math.max(40, Math.round(pin.height * pin.zoom));
  const win = new WebviewWindow(label, {
    url: `/?w=pin&pin=${pin.id}`,
    title: "NexusForge 贴图",
    decorations: false,
    transparent: false,
    alwaysOnTop: true,
    skipTaskbar: true,
    shadow: true,
    resizable: false,
  });
  win.once("tauri://error", (e) => {
    console.error("贴图窗口创建失败", e);
  });
  win.once("tauri://created", async () => {
    try {
      await win.setPosition(new PhysicalPosition(pin.x, pin.y));
      await win.setSize(new PhysicalSize(w, h));
    } catch (e) {
      console.error("贴图窗口定位失败", e);
    }
  });
}

/** 启动恢复全部贴图（主窗口 mount 时调用；文件丢失的记录后端已过滤） */
export async function restorePins(): Promise<void> {
  if (!IN_TAURI) return;
  const { screenshotPins } = await import("../ipc/client");
  try {
    const pins = await screenshotPins();
    for (const pin of pins) {
      await openPinWindow(pin);
    }
  } catch {
    // 模块未就绪（safe-mode 等）时静默
  }
}
