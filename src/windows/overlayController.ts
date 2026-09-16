import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { emit } from "@tauri-apps/api/event";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import { IN_TAURI } from "../ipc/env";
import {
  hostLog,
  screenshotStart,
  screenshotDiscard,
  screenshotPins,
  type TaskStartDto,
} from "../ipc/client";

/**
 * 截图覆盖层控制器（docs/impl/03 P3，docs/UI-PLAN.md U5）。
 *
 * 预热路径（默认）：应用启动即预建隐藏覆盖层窗口，热键触发时
 * screenshot_start 抓帧 → nf:overlay:task 事件派发给覆盖层 →
 * 覆盖层自行定位/取帧/渲染完成后自显。感知延迟 ≈ 抓帧耗时，无 WebView 冷启动。
 * 回退路径：预热窗口缺失时按需创建（URL 携带 task 参数，旧链路）。
 * 关键失败经 hostLog 上报宿主日志（webview console 外部不可见）。
 */

/** 创建中互斥：防止事件双发（React StrictMode 双监听）时重复建窗/重复抓帧 */
let creating = false;
/** 预热去重（先于 await 置位，防 StrictMode 双挂载竞态） */
let prewarmed = false;
/** 贴图恢复去重（StrictMode 双挂载会并发调 restorePins，getByLabel 竞态导致重复建窗） */
let restoring = false;

/** 应用启动时预建隐藏覆盖层窗口（MainWorkbench mount 调用） */
export async function prewarmOverlay(): Promise<void> {
  if (!IN_TAURI || prewarmed) return;
  prewarmed = true;
  try {
    const existing = await WebviewWindow.getByLabel("overlay");
    if (existing) return;
    const win = new WebviewWindow("overlay", {
      url: "/?w=overlay",
      title: "NexusForge 截图",
      decorations: false,
      transparent: false,
      alwaysOnTop: true,
      skipTaskbar: true,
      shadow: false,
      resizable: false,
      visible: false, // 常驻隐藏，等任务事件
    });
    win.once("tauri://error", (e) => {
      prewarmed = false; // 允许下次重试
      hostLog("error", `prewarmOverlay: 覆盖层窗口创建失败: ${JSON.stringify(e)}`);
    });
  } catch (e) {
    prewarmed = false;
    hostLog("warn", `prewarmOverlay 失败: ${JSON.stringify(e)}`);
  }
}

export async function startOverlay(mode: "shot" | "ocr"): Promise<void> {
  if (!IN_TAURI) return;
  if (creating) return;
  creating = true; // 先于任何 await 置位，防事件双发竞态
  try {
    let info: TaskStartDto;
    try {
      info = await screenshotStart(mode);
    } catch (e) {
      hostLog("error", `startOverlay: screenshot_start 失败 mode=${mode}: ${JSON.stringify(e)}`);
      return;
    }
    const existing = await WebviewWindow.getByLabel("overlay");
    if (existing) {
      // 预热路径：派发任务，覆盖层自行定位/取帧/自显（内容就绪才 show，无黑帧）
      await emit("nf:overlay:task", info);
      return;
    }
    // 回退路径：按需创建（URL 传参旧链路，覆盖层 mount 时自行装载）
    hostLog("info", `startOverlay: 任务 ${info.task_id} 抓帧成功 ${info.width}x${info.height}，创建覆盖层窗口`);
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
      hostLog("error", `startOverlay: 覆盖层窗口创建失败: ${JSON.stringify(e)}`);
    });
    win.once("tauri://created", async () => {
      try {
        // 物理像素显式定位（多显示器/高 DPI 下与抓帧坐标系一致）
        await win.setPosition(new PhysicalPosition(info.x, info.y));
        await win.setSize(new PhysicalSize(info.width, info.height));
        await win.show();
        await win.setFocus();
        hostLog("info", "startOverlay: 覆盖层窗口已显示");
      } catch (e) {
        hostLog("error", `startOverlay: 覆盖层定位失败: ${JSON.stringify(e)}`);
      }
    });
  } finally {
    creating = false;
  }
}

/** 取消任务：丢弃帧 + 隐藏覆盖层（隐藏而非关闭，保留预热窗口供下次秒开） */
export async function cancelOverlay(taskId: string | null): Promise<void> {
  if (taskId) await screenshotDiscard(taskId).catch(() => undefined);
  const overlay = await WebviewWindow.getByLabel("overlay");
  await overlay?.hide();
}

/** Pin 贴图窗口 label（唯一性；必须用连字符以匹配 capabilities 的 pin-* glob） */
function pinLabel(id: string): string {
  return `pin-${id.replace(/-/g, "")}`;
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
    hostLog("error", `openPinWindow: 贴图窗口创建失败 ${label}: ${JSON.stringify(e)}`);
  });
  win.once("tauri://created", async () => {
    try {
      await win.setPosition(new PhysicalPosition(pin.x, pin.y));
      await win.setSize(new PhysicalSize(w, h));
    } catch (e) {
      hostLog("error", `openPinWindow: 贴图窗口定位失败 ${label}: ${JSON.stringify(e)}`);
    }
  });
}

/** 启动恢复全部贴图（主窗口 mount 时调用；文件丢失的记录后端已过滤） */
export async function restorePins(): Promise<void> {
  if (!IN_TAURI || restoring) return;
  restoring = true; // 先于 await 置位，防 StrictMode 双挂载竞态
  try {
    const pins = await screenshotPins();
    for (const pin of pins) {
      await openPinWindow(pin);
    }
  } catch (e) {
    hostLog("warn", `restorePins 失败: ${JSON.stringify(e)}`);
  } finally {
    restoring = false;
  }
}
