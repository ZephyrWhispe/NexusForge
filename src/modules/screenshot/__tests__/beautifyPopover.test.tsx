/**
 * 美化导出（09 §9.2 T-B4-6）前端两面：BeautifyPopover 的预览/预设语义 + 历史行的美化入口。
 *
 * 红线在这里的形状是"预览不许假装落盘"：预览臂 actions 恒为空数组，且成功后
 * 一条 notify 都不许发（发了"已保存"就是骗）；预设 chip 点下去填表并**只**取一次预览。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";

import BeautifyPopover, { BEAUTIFY_PRESETS, toBeautifySpec } from "../BeautifyPopover";
import ScreenshotPanel from "../ScreenshotPanel";
import { notify } from "../../../stores/notifications";
import {
  screenshotBeautifyApply,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  type ShotItemDto,
} from "../../../ipc/client";

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotBeautifyApply: vi.fn(),
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

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.body.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

function inputByLabel(label: string): HTMLInputElement | null {
  return document.body.querySelector(`input[aria-label="${label}"]`);
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
    format: "image/png",
    annotations: [],
  }));
  vi.mocked(screenshotHistoryList).mockResolvedValue({
    items: [shot("s1", "C:\\shots\\shot_1.png"), shot("s2", null)],
    total: 2,
    page: 1,
    size: 30,
  });
  vi.mocked(screenshotBeautifyApply).mockResolvedValue({
    file: "C:\\shots\\shot_1_beautified.png",
    pin_id: null,
    preview_b64: "QUJD",
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
  // Fluent 门户节点逐个摘除：给 body 整体赋 innerHtml 字符串会被 B3 的注入面红线
  // 扫描器（htmlFormat.test.tsx 的 PREEXISTING_SINKS 精确集合断言）判成新增宿主。
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function render(node: ReactNode) {
  await act(async () => {
    root = createRoot(container);
    root.render(node);
  });
  await act(async () => {});
}

function menuItems(): HTMLElement[] {
  return [...document.body.querySelectorAll<HTMLElement>('[role="menuitem"]')];
}

describe("BeautifyPopover（T-B4-6）", () => {
  it("beautifyPopover_previewInvokesWithEmptyActions_onlyPreviewNoSave", async () => {
    await render(
      <BeautifyPopover
        target={{ id: "s1", label: "美化另存", actions: ["save"] }}
        onClose={() => {}}
      />,
    );
    const previewBtn = buttonByText("预览");
    expect(previewBtn, "预览钮必须在").toBeTruthy();
    await act(async () => {
      previewBtn!.click();
    });
    // 字面判据：空 actions = 宿主侧零落盘零剪贴板写
    expect(screenshotBeautifyApply).toHaveBeenCalledWith(
      "s1",
      expect.objectContaining({ radius: 24, padding: 32, shadow: true, bg_from: "#1f2937" }),
      [],
    );
    // 预览图就地点亮（base64 由 preview_b64 来）
    const img = container.querySelector("img");
    expect(img?.getAttribute("src")).toBe("data:image/png;base64,QUJD");
    // 红线：预览一声不响——不发 notify，屏幕上也不许出现"已保存"字样
    expect(notify).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("已保存");
    // 正对照：导出臂才带动作
    await act(async () => {
      buttonByText("美化另存")!.click();
    });
    expect(screenshotBeautifyApply).toHaveBeenLastCalledWith(
      "s1",
      expect.objectContaining({ radius: 24 }),
      ["save"],
    );
    expect(notify).toHaveBeenCalledTimes(1);
  });

  it("beautifyPopover_presetChips_fillSpecFieldsAndInvokeOnce", async () => {
    await render(
      <BeautifyPopover
        target={{ id: "s9", label: "美化复制", actions: ["copy"] }}
        onClose={() => {}}
      />,
    );
    expect(BEAUTIFY_PRESETS.map((p) => p.label)).toEqual(["无", "圆角阴影", "社交卡片"]);
    const social = BEAUTIFY_PRESETS[2];
    await act(async () => {
      buttonByText("社交卡片")!.click();
    });
    // 一次点击 = 填表 + 恰好一次预览（多调一次就是每次点 chip 都去读盘取图）
    expect(screenshotBeautifyApply).toHaveBeenCalledTimes(1);
    expect(screenshotBeautifyApply).toHaveBeenCalledWith(
      "s9",
      toBeautifySpec(social.fields),
      [],
    );
    expect(inputByLabel("圆角半径")?.value).toBe("40");
    expect(inputByLabel("内边距")?.value).toBe("96");
    expect(inputByLabel("渐变起色")?.value).toBe("#7c3aed");
    expect(inputByLabel("渐变止色")?.value).toBe("#0ea5e9");
    // 恒等预设臂：点"无"把五格全归零（负对照，防 chip 只改颜色不改形状）
    await act(async () => {
      buttonByText("无")!.click();
    });
    expect(screenshotBeautifyApply).toHaveBeenLastCalledWith(
      "s9",
      { radius: 0, padding: 0, shadow: false, bg_from: "#000000", bg_to: "#000000" },
      [],
    );
    expect(inputByLabel("圆角半径")?.value).toBe("0");
  });
});

describe("历史行美化菜单（T-B4-6）", () => {
  it("shotPanel_beautifyMenu_twoItemsPerRow", async () => {
    await render(<ScreenshotPanel />);
    expect(container.querySelector("input[aria-label='圆角半径']"), "未选目标时不渲染弹层").toBeNull();
    const triggers = [...container.querySelectorAll("button")].filter(
      (b) => b.textContent?.trim() === "美化",
    );
    // 每行一枚入口（两行历史 = 两枚），不是全局共享一枚
    expect(triggers).toHaveLength(2);
    // 红线：没有字节的行（file 为 null）整只菜单禁用——点开只能报错
    expect(triggers[1].disabled).toBe(true);
    await act(async () => {
      triggers[0].click();
    });
    const items = menuItems().map((i) => i.textContent?.trim());
    expect(items).toEqual(["美化另存", "美化复制"]);
    await act(async () => {
      menuItems()
        .find((i) => i.textContent?.trim() === "美化复制")!
        .click();
    });
    // 选完即出弹层，导出臂带的是刚选的那个动作
    expect(inputByLabel("圆角半径")).toBeTruthy();
    await act(async () => {
      buttonByText("美化复制")!.click();
    });
    expect(screenshotBeautifyApply).toHaveBeenLastCalledWith(
      "s1",
      expect.anything(),
      ["copy"],
    );
  });
});
