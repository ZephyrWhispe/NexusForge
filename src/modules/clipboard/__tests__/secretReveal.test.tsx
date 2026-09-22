import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardCaptureGet,
  clipboardClear,
  clipboardGet,
  clipboardGroupCounts,
  clipboardPaste,
  clipboardSearch,
  clipboardSecretReveal,
  type ClipEntry,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";
import { useSession } from "../../../stores/session";

// D-29 B3/T-B3-5 回归（09 §8.2）：敏感库是掩码清单 + 需二次确认的揭示口。
// 通用读口 clipboard_get 自本行起对敏感行返回 CLIPBOARD_GET_001，故前端任何
// 一处拿敏感行走 plain 读口都会当场判红——这正是"读口比写口宽"缺陷的收口点。

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
    clipboardCaptureGet: vi.fn(),
    clipboardSecretReveal: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

// jsdom 无 ResizeObserver/真实 rect，虚拟器观测不到滚动容器就不产行（同 pasteStack.test 处置）
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 64,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({ index: i, start: i * 64, key: i, size: 64 })),
    measureElement: () => {},
  }),
}));

const MASK = "[敏感内容] 已加密存储";
const PLAIN = "sk-revealed-9f3a1c0b2d4e6f8a";

function clip(id: string, over: Partial<ClipEntry> = {}): ClipEntry {
  return {
    id,
    content_type: "text",
    preview: MASK,
    blob_path: null,
    origin: "local",
    source_app: "Browser",
    pinned: false,
    group: null,
    secret: true,
    created_at: Date.parse("2026-09-22T09:10:00"),
    usage_count: 0,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("sec1")],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardSecretReveal).mockResolvedValue({ id: "sec1", text: PLAIN });
  vi.mocked(clipboardGet).mockResolvedValue("普通读口不该为敏感行返回东西");
  vi.mocked(clipboardGroupCounts).mockResolvedValue({ secret: 1 });
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardClear).mockResolvedValue(0);
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
  vi.clearAllMocks();
  useSession.setState({ clipView: "history", clipGroup: "all" });
});

async function mount(view: "history" | "secret") {
  act(() => useSession.getState().setClipView(view));
  await act(async () => {
    root = createRoot(container);
    root.render(<ClipboardPanel search="" group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
}

async function click(el: Element | undefined | null) {
  if (!el) throw new Error("目标控件未渲染");
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

const buttonByText = (text: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
const slot = () => container.querySelector('[aria-label="内容区"]');

describe("敏感库揭示门（T-B3-5）", () => {
  it("secretSection_reveal_requiresConfirmThenShowsText：确认在前、揭示在后，明文只出现在 pre", async () => {
    await mount("secret");
    expect(clipboardSearch).toHaveBeenCalledWith(
      expect.objectContaining({ group: "secret" }),
    );
    expect(container.textContent).toContain(MASK);
    expect(container.querySelector("pre")).toBeNull();

    await click(buttonByText("揭示"));
    expect(confirmAction).toHaveBeenCalledWith(
      expect.objectContaining({ danger: true, title: "揭示敏感内容", command: "sec1" }),
    );
    expect(clipboardSecretReveal).toHaveBeenCalledWith("sec1");
    // 通用读口一次都不走：明文只从揭示口出
    expect(clipboardGet).not.toHaveBeenCalled();
    expect(container.querySelector("pre")?.textContent).toBe(PLAIN);

    // 「复制」复制的是已揭示文本（浏览器剪贴板），不是绕道 clipboard_paste
    const writeText = vi.fn(async () => {});
    Object.defineProperty(globalThis.navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    await click(buttonByText("复制"));
    expect(writeText).toHaveBeenCalledWith(PLAIN);
    expect(clipboardPaste).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith(
      "success",
      "已复制已揭示文本",
      expect.stringContaining("非粘贴口"),
    );
  });

  it("secretSection_cancel_zeroInvoke：确认框取消即零揭示调用、掩码不变", async () => {
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await mount("secret");
    await click(buttonByText("揭示"));
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(clipboardSecretReveal).not.toHaveBeenCalled();
    expect(container.textContent).toContain(MASK);
    expect(container.textContent).not.toContain(PLAIN);
    expect(buttonByText("揭示")).toBeDefined();
  });

  it("secretSection_maskStableHeightAfterReveal：掩码与明文共用同一固定高容器，切换不跳版", async () => {
    await mount("secret");
    const before = slot();
    expect(before).not.toBeNull();
    const clsBefore = before!.className;
    expect(before!.textContent).toContain(MASK);

    await click(buttonByText("揭示"));

    const after = slot();
    // 同一 DOM 节点被复用（不是拆掉重建），且容器类名未随状态改变
    expect(after).toBe(before);
    expect(after!.className).toBe(clsBefore);
    expect(after!.textContent).toContain(PLAIN);
    // 收起：明文即收回，容器仍是同一个
    await click(buttonByText("收起"));
    expect(container.textContent).not.toContain(PLAIN);
    expect(slot()).toBe(after);
  });

  it("historyDetail_secretRow_neverCallsPlainGet：历史详情对敏感行零 clipboard_get，指路敏感库", async () => {
    await mount("history");
    await click(container.querySelector('[aria-label="详情"]'));
    expect(clipboardGet).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain(MASK);
    expect(document.body.textContent).toContain("敏感库");
    expect(document.body.textContent).not.toContain(PLAIN);

    // 正对照：普通条目同一路径照旧走 clipboard_get（否则上面那臂只是"详情坏了"）
    vi.mocked(clipboardSearch).mockResolvedValue({
      items: [clip("plain1", { preview: "普通一行", secret: false })],
      has_more: false,
      total: null,
    });
    vi.mocked(clipboardGet).mockResolvedValue("普通一行全文");
    act(() => {
      root?.unmount();
    });
    root = null;
    await mount("history");
    await click(container.querySelector('[aria-label="详情"]'));
    expect(clipboardGet).toHaveBeenCalledWith("plain1");
    expect(document.body.textContent).toContain("普通一行全文");
  });
});
