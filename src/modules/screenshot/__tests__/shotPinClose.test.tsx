/**
 * 贴图行「关闭」（D-42 二级窗独占能力入主窗）：主面板此前只贴缩略图，
 * 想收掉一枚贴图必须走到那枚贴图自己的浮窗里 Esc/双击——面板这条腿是断的。
 *
 * 三道判据沿用历史行删除（shotDelete.test.tsx）的在册口径：
 * ① 不可逆先确认（`screenshot_pin_close` 连 `pins/{id}.png` 一起删，不进回收站）；
 * ② 取消臂零 invoke、连重取都不做；
 * ③ 成功后重取列表信宿主。
 * 另钉一枚本批新增的语义：后端 `pin_close` 只删记录与文件、不碰前端窗口
 * （crates/screenshot-core/src/module.rs:1401-1420），面板必须补一次 closePinWindow，
 * 否则屏幕上留一枚还在显示已删图片的幽灵窗。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import { confirmAction } from "../../../stores/confirm";
import { closePinWindow } from "../../../windows/overlayController";
import {
  hostConfigGet,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinClose,
  screenshotPinGet,
  screenshotPins,
  screenshotUploadTargets,
  type PinDataDto,
} from "../../../ipc/client";

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    hostConfigGet: vi.fn(),
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotHistoryDelete: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotPinClose: vi.fn(),
    screenshotUploadTargets: vi.fn(),
  };
});

// 贴图窗生命周期腿（真模块静态载 @tauri-apps/api/window，jsdom 下只需断言被调）
vi.mock("../../../windows/overlayController", () => ({
  closePinWindow: vi.fn(async () => {}),
  startOverlay: vi.fn(async () => {}),
  openPinWindow: vi.fn(async () => {}),
}));

const PIN: PinDataDto = {
  id: "pin-1",
  x: 40,
  y: 60,
  width: 320,
  height: 180,
  zoom: 1.5,
  opacity: 0.8,
  png_b64: "AA",
};

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(confirmAction).mockImplementation(async () => true);
  vi.mocked(hostConfigGet).mockResolvedValue({});
  vi.mocked(screenshotUploadTargets).mockResolvedValue([]);
  vi.mocked(screenshotHistoryList).mockResolvedValue({ items: [], total: 0, page: 1, size: 30 });
  vi.mocked(screenshotHistoryGet).mockResolvedValue({
    id: "s",
    png_b64: "QUJD",
    format: "image/png",
    annotations: [],
  });
  vi.mocked(screenshotPins).mockResolvedValue([PIN]);
  vi.mocked(screenshotPinGet).mockResolvedValue(PIN);
  vi.mocked(screenshotPinClose).mockResolvedValue(undefined);
  vi.mocked(closePinWindow).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  root = null;
  container.remove();
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

async function clickClose(): Promise<void> {
  const el = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === "关闭",
  );
  if (!el) throw new Error("贴图行没有「关闭」按钮");
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

describe("截图面板 · 贴图行关闭（D-42）", () => {
  it("shotPanel_pinRow_showsGeometryZoomAndOpacity", async () => {
    await mount();
    // zoom/opacity 随 screenshot_pin_get 下发却从未上屏（PinDto 死字段之一），本批补显影
    expect(bodyText()).toContain("320×180");
    expect(bodyText()).toContain("缩放 150%");
    expect(bodyText()).toContain("不透明度 80%");
  });

  it("shotPanel_pinClose_cancelArmZeroInvoke", async () => {
    vi.mocked(confirmAction).mockImplementation(async () => false);
    await mount();
    const listsBefore = vi.mocked(screenshotPins).mock.calls.length;
    await clickClose();

    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(screenshotPinClose).not.toHaveBeenCalled();
    expect(closePinWindow).not.toHaveBeenCalled();
    expect(vi.mocked(screenshotPins).mock.calls.length, "取消=什么都没变，刷新是假动作").toBe(
      listsBefore,
    );
  });

  it("shotPanel_pinClose_deletesRecordAndClosesWindowThenRefetches", async () => {
    await mount();
    // 宿主那一侧"还没落下手"：重取仍回这一条，面板就得继续显示它（不本地滤行的代价＝诚实）
    await clickClose();
    expect(screenshotPinClose).toHaveBeenCalledWith("pin-1");
    // 后端只管记录与文件，窗口这一步漏了就会留一枚显示已删图片的幽灵窗
    expect(closePinWindow).toHaveBeenCalledWith("pin-1");
    expect(vi.mocked(screenshotPins).mock.calls.length, "删后重取，不由面板本地滤行").toBe(2);
    expect(bodyText()).toContain("缩放 150%");

    vi.mocked(screenshotPins).mockResolvedValue([]);
    await clickClose();
    expect(vi.mocked(screenshotPins).mock.calls.length).toBe(3);
    expect(bodyText()).not.toContain("缩放 150%");
  });
});
