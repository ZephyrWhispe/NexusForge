/**
 * 窗口捕获与枚举（09 §9.2 T-B4-4）：面板「截取窗口」下拉。
 *
 * 钉三件事：
 * ① 窗口表**点开才读**（枚举别的应用的窗口标题是跨应用隐私面，不该在面板挂载时顺手做）；
 * ② 最小化窗不进选项，但空表文案要分得清"一个都没有"和"都被最小化了"；
 * ③ 读失败 ≠ 读到了空表——两种沉默长得一样，处置完全不同。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import { startOverlay } from "../../../windows/overlayController";
import {
  hostConfigGet,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  screenshotWindows,
  type WindowTargetDto,
} from "../../../ipc/client";

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));
vi.mock("../../../windows/overlayController", () => ({
  startOverlay: vi.fn(),
  prewarmOverlay: vi.fn(),
  openPinWindow: vi.fn(),
  restorePins: vi.fn(),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotWindows: vi.fn(),
  };
});

function win(partial: Partial<WindowTargetDto> & { hwnd: number; title: string }): WindowTargetDto {
  return { x: 0, y: 0, width: 800, height: 600, minimized: false, ...partial };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  // 真正的挂载容器由 mount() 每次新建（本用例集里一个测试会挂卸四轮）
  container = document.createElement("div");
  vi.mocked(hostConfigGet).mockResolvedValue({ post_actions: [], save_dir: "" });
  vi.mocked(screenshotHistoryList).mockResolvedValue({ items: [], total: 0, page: 1, size: 30 });
  vi.mocked(screenshotHistoryGet).mockResolvedValue({
    id: "x",
    png_b64: "QUJD",
    format: "image/png",
    annotations: [],
  });
  vi.mocked(screenshotPins).mockResolvedValue([]);
  vi.mocked(screenshotPinGet).mockResolvedValue({
    id: "p",
    x: 0,
    y: 0,
    width: 1,
    height: 1,
    zoom: 1,
    opacity: 1,
    png_b64: "AA",
  });
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  document.body.replaceChildren();
  vi.clearAllMocks();
});

async function mount() {
  container = document.createElement("div");
  document.body.append(container);
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  await act(async () => {});
}

function unmount() {
  act(() => root.unmount());
  document.body.replaceChildren();
}

function buttonByText(text: string): HTMLButtonElement {
  const btn = [...container.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(text),
  );
  if (!btn) throw new Error(`找不到文案含「${text}」的按钮`);
  return btn as HTMLButtonElement;
}

/** 点开「截取窗口」菜单（受控 open：点击 → onOpenChange → 现读窗口表） */
async function openMenu() {
  await act(async () => {
    buttonByText("截取窗口").click();
  });
}

/** 全页（含 portal 出去的浮层）里文案含 needle 的可点项 */
function itemByText(needle: string): HTMLElement | undefined {
  return [...document.body.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((el) =>
    (el.textContent ?? "").includes(needle),
  );
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

describe("截图面板 · 截取窗口下拉（T-B4-4）", () => {
  it("shotPanel_windowDropdown_listsTitleAndSize_andSkipsMinimized", async () => {
    vi.mocked(screenshotWindows).mockResolvedValue([
      win({ hwnd: 4242, title: "此电脑", width: 800, height: 600 }),
      win({ hwnd: 7, title: "收件箱", width: 1024, height: 768, minimized: true }),
    ]);
    await mount();
    // 点开之前一次都不该枚举（挂载即枚举＝每次进面板都翻一遍别人的窗口标题）
    expect(screenshotWindows).not.toHaveBeenCalled();

    await openMenu();
    expect(itemByText("此电脑 · 800×600")).toBeTruthy();
    // 最小化那行既不在项里、也不该被"没有可截取的窗口"文案替身出现（标题排掉、计数留住）
    expect(itemByText("收件箱")).toBeFalsy();
    expect(bodyText()).not.toContain("没有可截取的窗口");
  });

  it("shotPanel_windowPick_invokesStartWithHwnd", async () => {
    vi.mocked(screenshotWindows).mockResolvedValue([win({ hwnd: 4242, title: "此电脑" })]);
    await mount();
    await openMenu();
    const item = itemByText("此电脑");
    if (!item) throw new Error("菜单项没渲染出来");
    await act(async () => {
      item.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(startOverlay).toHaveBeenCalledWith("shot", 4242);
  });

  it("shotPanel_windowsEmpty_honestGuideNotSilent", async () => {
    // ① 真·空表：说"没有可截取的窗口"，且没有可点项（不弹静默空框）
    vi.mocked(screenshotWindows).mockResolvedValue([]);
    await mount();
    await openMenu();
    expect(bodyText()).toContain("没有可截取的窗口");
    expect(itemByText("此电脑")).toBeFalsy();
    expect(startOverlay).not.toHaveBeenCalled();
    unmount();

    // ② 全被最小化：文案点名数量（"恢复窗口"这一步用户自己能做，不该以为程序坏了）
    vi.mocked(screenshotWindows).mockResolvedValue([
      win({ hwnd: 7, title: "收件箱", minimized: true }),
      win({ hwnd: 8, title: "日历", minimized: true }),
    ]);
    await mount();
    await openMenu();
    expect(bodyText()).toContain("2 个窗口已最小化");
    unmount();

    // 正对照：同一套代码，掺一个可见窗就不再走引导文案（否则上面两条可以是"永远显示引导"）
    vi.mocked(screenshotWindows).mockResolvedValue([
      win({ hwnd: 4242, title: "此电脑" }),
      win({ hwnd: 7, title: "收件箱", minimized: true }),
    ]);
    await mount();
    await openMenu();
    expect(bodyText()).not.toContain("没有可截取的窗口");
    expect(bodyText()).not.toContain("已最小化");
    expect(itemByText("此电脑")).toBeTruthy();
    unmount();

    // ③ 读失败 ≠ 读到了空表：说的是另一句话（同一句话会把"权限没给"混成"你没开窗口"）
    vi.mocked(screenshotWindows).mockRejectedValue(new Error("no acl"));
    await mount();
    await openMenu();
    expect(bodyText()).toContain("窗口枚举失败");
    expect(bodyText()).not.toContain("没有可截取的窗口");
    expect(startOverlay).not.toHaveBeenCalled();
  });
});
