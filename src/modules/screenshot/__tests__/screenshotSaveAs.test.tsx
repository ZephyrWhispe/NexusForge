/**
 * 历史行"另存为"溢出菜单（09 §9.2 T-B4-7）：三枚格式项 + 点击带的是那一条记录。
 *
 * 编码本身（canvas 重编码 + `<a download>`）不在这里测——jsdom 没有 2d 上下文，
 * 那条路的判据属批次尾实启冒烟（人工）；本文件钉的是**入口形状**：三枚齐全、
 * 禁用条件、以及点 JPEG 项时真的把 `("s1", 文件, "jpeg")` 交给了 saveAs。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import { saveShotAs } from "../saveAs";
import {
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  type ShotItemDto,
} from "../../../ipc/client";

vi.mock("../saveAs", () => ({ saveShotAs: vi.fn() }));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
  };
});

function shot(id: string, file: string | null): ShotItemDto {
  return {
    id,
    created_ms: Date.parse("2026-09-19T10:30:00"),
    width: 100,
    height: 50,
    file,
    ocr_text: null,
  };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
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
  vi.mocked(screenshotHistoryGet).mockImplementation(async (id) => ({
    id,
    png_b64: "QUJD",
    format: "image/jpeg",
    annotations: [],
  }));
  vi.mocked(screenshotHistoryList).mockResolvedValue({
    items: [shot("s1", "C:\\shots\\shot_1.png"), shot("s2", null)],
    total: 2,
    page: 1,
    size: 30,
  });
  vi.mocked(saveShotAs).mockResolvedValue("shot_1.jpg");
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
  // Fluent 菜单在 body 上留门户节点，逐个摘掉而不是重写 innerHTML：
  // 后者会被 B3 的注入面红线扫描器（htmlFormat.test.tsx）当成新增宿主判红。
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

function menuTriggers(): HTMLButtonElement[] {
  return [...container.querySelectorAll("button")].filter((b) =>
    b.textContent?.includes("另存为"),
  ) as HTMLButtonElement[];
}

async function openMenu(idx: number) {
  await act(async () => {
    menuTriggers()[idx]?.click();
  });
}

function menuItems(): HTMLElement[] {
  return [...document.body.querySelectorAll('[role="menuitem"]')] as HTMLElement[];
}

describe("历史行另存为菜单（T-B4-7）", () => {
  it("shotPanel_saveAsMenu_threeFormats", async () => {
    await mount();
    // 每行一枚入口（两行=两枚），而不是全局一枚共享菜单
    expect(menuTriggers()).toHaveLength(2);
    await openMenu(0);
    expect(menuItems().map((i) => i.textContent)).toEqual(["PNG", "JPEG", "WebP"]);
    const jpeg = menuItems().find((i) => i.textContent === "JPEG")!;
    await act(async () => {
      jpeg.click();
    });
    // 字面判据：点的那一行 + 选的那一档，两样都得带上
    expect(saveShotAs).toHaveBeenCalledWith("s1", "C:\\shots\\shot_1.png", "jpeg");
  });

  it("shotPanel_saveAsMenu_rowWithoutFile_disabled", async () => {
    await mount();
    const triggers = menuTriggers();
    expect(triggers[0].disabled).toBe(false);
    // 未保存过文件的行：没有字节可下载，入口必须禁用而不是点开报错
    expect(triggers[1].disabled).toBe(true);
    await openMenu(1);
    expect(menuItems()).toHaveLength(0);
    expect(saveShotAs).not.toHaveBeenCalled();
  });

  it("缩略图 src 用嗅探 MIME 而不是写死 png", async () => {
    await mount();
    // 夹具回的是 image/jpeg：写死 data:image/png 的那一版在这里就会红
    const srcs = [...container.querySelectorAll("img")].map((i) => i.getAttribute("src"));
    expect(srcs).toContain("data:image/jpeg;base64,QUJD");
    expect(srcs.some((s) => s?.startsWith("data:image/png"))).toBe(false);
  });
});
