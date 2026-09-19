import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel, { formatShotTime, shotMatchesFilter } from "../ScreenshotPanel";
import {
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  type ShotItemDto,
} from "../../../ipc/client";

// D-29 B0/T-B0-2 回归：以 mock 的 IPC 出入口渲染真组件，
// 钉死"历史行来自后端数据"与"真空库 ≠ 本页筛选无果"两种文案（00§4-4）。

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

function shot(id: string, w: number, h: number, ocr: string | null = null): ShotItemDto {
  return { id, created_ms: Date.parse("2026-09-19T10:30:00"), width: w, height: h, file: `/x/${id}.png`, ocr_text: ocr };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(screenshotPins).mockResolvedValue([]);
  vi.mocked(screenshotPinGet).mockResolvedValue({
    id: "p", x: 0, y: 0, width: 1, height: 1, zoom: 1, opacity: 1, png_b64: "AA",
  });
  vi.mocked(screenshotHistoryGet).mockImplementation(async (id) => ({ id, png_b64: "QUJD" }));
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
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  // 再冲刷一轮：历史列表落定后触发的缩略图取字节
  await act(async () => {});
}

describe("ScreenshotPanel（T-B0-2）", () => {
  it("screenshotPanel_rendersHistoryRows_fromMockedInvoke：行与缩略图来自后端数据", async () => {
    vi.mocked(screenshotHistoryList).mockResolvedValue({
      items: [shot("s1", 1920, 1080, "登录验证码"), shot("s2", 800, 600)],
      total: 2,
      page: 1,
      size: 30,
    });
    await mount();
    expect(screenshotHistoryList).toHaveBeenCalledWith(1, 30);
    expect(container.textContent).toContain("1920×1080");
    expect(container.textContent).toContain("800×600");
    expect(container.textContent).toContain("登录验证码");
    expect(container.textContent).toContain("09-19 10:30");
    expect(container.textContent).toContain("2 条历史");
    const imgs = [...container.querySelectorAll("img")];
    expect(imgs.map((i) => i.getAttribute("src"))).toContain("data:image/png;base64,QUJD");
    // 缩略图逐条按 id 取字节（非整页打包事件）
    expect(screenshotHistoryGet).toHaveBeenCalledWith("s1");
    expect(screenshotHistoryGet).toHaveBeenCalledWith("s2");
  });

  it("screenshotPanel_emptyVsNoResult_twoCopy：真空库引导 ≠ 筛选无果提示", async () => {
    // 空库：给出首动作引导，不得出现"匹配"字样
    vi.mocked(screenshotHistoryList).mockResolvedValue({ items: [], total: 0, page: 1, size: 30 });
    await mount();
    expect(container.textContent).toContain("还没有截图记录");
    expect(container.textContent).not.toContain("没有匹配");
    act(() => root.unmount());
    container = document.createElement("div");
    document.body.append(container);

    // 有数据但本页筛选无果：如实说明范围，不冒充空库
    vi.mocked(screenshotHistoryList).mockResolvedValue({
      items: [shot("s1", 100, 100, "alpha")],
      total: 1,
      page: 1,
      size: 30,
    });
    await mount();
    const input = container.querySelector("input");
    expect(input).not.toBeNull();
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(input, "zzz-miss");
      input!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.textContent).toContain("没有匹配");
    expect(container.textContent).toContain("本页 1 条");
    expect(container.textContent).not.toContain("还没有截图记录");
  });

  it("纯函数判据：formatShotTime / shotMatchesFilter（可失败自检）", () => {
    expect(formatShotTime(Date.parse("2026-09-19T07:05:00"))).toBe("09-19 07:05");
    const s = shot("a", 1920, 1080, "Hello 世界");
    expect(shotMatchesFilter(s, "")).toBe(true);
    expect(shotMatchesFilter(s, "hello")).toBe(true); // OCR 文本，大小写不敏感
    expect(shotMatchesFilter(s, "1920x1080")).toBe(true); // 尺寸串
    expect(shotMatchesFilter(s, ".png")).toBe(true); // 文件名
    expect(shotMatchesFilter(s, "nomatch")).toBe(false);
  });
});
