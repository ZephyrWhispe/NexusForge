import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import { clipboardClear, clipboardGet, clipboardSearch, type ClipEntry } from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

// D-29 B1/T-B1-1+2 回归：工具栏「清空」经面板局部 Dialog 二次确认（保留置顶 Checkbox
// 必须真实进 clipboardClear 参数），行「详情」必须真实 invoke clipboard_get 且空串
// 用诚实合并文案（clipboard.rs:23 unwrap_or_default 对缺行/空内容不可分辨）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    clipboardSearch: vi.fn(),
    clipboardClear: vi.fn(),
    clipboardGet: vi.fn(),
    clipboardPaste: vi.fn(),
    clipboardPin: vi.fn(),
    clipboardDelete: vi.fn(),
    clipboardGroupCounts: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

// jsdom 无 ResizeObserver/真实 rect，useVirtualizer 观测不到滚动容器就不渲染任何
// 行（探针实测：仅工具栏在 DOM）。被测对象是面板→IPC 接线而非第三方布局机制，
// 故以直通假实现按 count 全量展开虚拟行。
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 64,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({ index: i, start: i * 64, key: i, size: 64 })),
    measureElement: () => {},
  }),
}));

function clip(id: string, over: Partial<ClipEntry> = {}): ClipEntry {
  return {
    id,
    content_type: "text",
    preview: "短预览",
    blob_path: null,
    origin: "local",
    source_app: "Notepad",
    pinned: false,
    group: null,
    secret: false,
    created_at: Date.parse("2026-09-20T07:05:00"),
    usage_count: 0,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}
function checkbox(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>('input[type="checkbox"]');
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("c1", { preview: "完整内容第一行" })],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardClear).mockResolvedValue(0);
  vi.mocked(clipboardGet).mockResolvedValue("");
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
    root.render(<ClipboardPanel search="" group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

describe("ClipboardPanel 工具栏清空 + 行详情（T-B1-1/2）", () => {
  it("clipboardToolbar_clear_confirmSendsKeepPinned：确认→clipboardClear(true) 恰一次+删除数 toast", async () => {
    vi.mocked(clipboardClear).mockResolvedValue(7);
    await mount();
    await click(buttonByText("清空")!);
    // 面板局部 Dialog：保留置顶 Checkbox 默认真实勾选（保守默认=不删置顶）
    const cb = checkbox();
    expect(cb).not.toBeNull();
    expect(cb?.checked).toBe(true);
    expect(document.body.textContent).toContain("保留置顶条目");
    await click(buttonByText("确认清空")!);
    expect(clipboardClear).toHaveBeenCalledTimes(1);
    expect(clipboardClear).toHaveBeenCalledWith(true);
    expect(notify).toHaveBeenCalledWith(
      "success",
      "已清空剪切板历史",
      expect.stringContaining("删除 7 条记录"),
    );
    // 列表重载由清空处理器自调（cleared 订阅在 jsdom 下不启用，不依赖它）
    expect(clipboardSearch).toHaveBeenCalledTimes(2);
  });

  it("clipboardToolbar_clear_cancelZeroInvoke：取消零 invoke（D-18 负例）", async () => {
    await mount();
    await click(buttonByText("清空")!);
    // 勾掉再勾回，确认控件交互真实但不产生任何调用
    const cb = checkbox();
    expect(cb).not.toBeNull();
    await click(cb!);
    expect(checkbox()?.checked).toBe(false);
    await click(buttonByText("取消")!);
    expect(clipboardClear).not.toHaveBeenCalled();
    expect(clipboardSearch).toHaveBeenCalledTimes(1); // 仅挂载首轮，无重载
  });

  it("clipboardDetail_fullTextViaGet：点详情真实 invoke clipboard_get 且全文渲染", async () => {
    vi.mocked(clipboardGet).mockResolvedValue("完整内容第一行\n第二行只有详情里才可见");
    await mount();
    const btn = container.querySelector('[aria-label="详情"]');
    expect(btn).not.toBeNull();
    await click(btn!);
    expect(clipboardGet).toHaveBeenCalledWith("c1");
    expect(document.body.textContent).toContain("第二行只有详情里才可见");
    // 全文渲染在可滚动 pre 中（非行内 ellipsis preview 复用）
    expect(document.querySelector("pre")?.textContent).toContain("完整内容第一行");
  });

  it("clipboardDetail_emptyString_honestCopy：空串渲染诚实合并文案，不二选一谎称", async () => {
    vi.mocked(clipboardGet).mockResolvedValue("");
    await mount();
    await click(container.querySelector('[aria-label="详情"]')!);
    expect(document.body.textContent).toContain("内容已空或记录已删");
    // 不得把两种后端不可分辨的状态各说成确定的一种
    expect(document.body.textContent).not.toContain("该记录不存在");
    expect(document.body.textContent).not.toContain("读取失败");
  });
});
