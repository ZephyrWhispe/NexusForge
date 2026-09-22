import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardCaptureGet,
  clipboardClear,
  clipboardGet,
  clipboardGroupCounts,
  clipboardSearch,
  clipboardStackClear,
  clipboardStackList,
  clipboardStackMove,
  clipboardStackPasteAll,
  clipboardStackPasteNext,
  clipboardStackPush,
  clipboardStackRemove,
  type ClipEntry,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";
import { useSession } from "../../../stores/session";

// D-29 B3/T-B3-3 回归（09 §8.2）：粘贴堆栈端到端——入栈来自历史行菜单，堆栈区负责
// 队列编辑与投递；投递前必须让出焦点（先隐藏本窗），否则注入的 Ctrl+V 落在自己身上。

/** 跨替身共享的调用轨迹：顺序即判据（hide 必先于 invoke，恢复必在其后） */
const seq: string[] = [];

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  const traced = (name: string, value: unknown) =>
    vi.fn(async () => {
      seq.push(name);
      return value;
    });
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
    clipboardStackPush: traced("stack_push", 1),
    clipboardStackList: vi.fn(),
    clipboardStackMove: traced("stack_move", undefined),
    clipboardStackRemove: traced("stack_remove", true),
    clipboardStackClear: traced("stack_clear", 2),
    clipboardStackPasteNext: traced("paste_next", {
      id: "s1",
      delivered: true,
      error: null,
    }),
    clipboardStackPasteAll: traced("paste_all", { delivered: 2, failed: 0, remaining: 0 }),
  };
});

const win = { hide: vi.fn(), show: vi.fn(), setFocus: vi.fn() };

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    hide: async () => {
      seq.push("hide");
      win.hide();
    },
    show: async () => {
      seq.push("show");
      win.show();
    },
    setFocus: async () => {
      seq.push("setFocus");
      win.setFocus();
    },
  }),
}));

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

// jsdom 无 ResizeObserver/真实 rect：虚拟器直通，行才渲染得出来
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
    preview: `预览-${id}`,
    blob_path: null,
    origin: "local",
    source_app: "Notepad",
    pinned: false,
    group: null,
    secret: false,
    has_html: false,
    created_at: Date.parse("2026-09-22T07:05:00"),
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
  seq.length = 0;
  vi.mocked(clipboardSearch).mockResolvedValue({ items: [clip("h1")], has_more: false, total: null });
  vi.mocked(clipboardStackList).mockResolvedValue([clip("s1"), clip("s2")]);
  vi.mocked(clipboardGroupCounts).mockResolvedValue({ text: 2 });
  vi.mocked(clipboardGet).mockResolvedValue("");
  vi.mocked(clipboardClear).mockResolvedValue(0);
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
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

async function mount(view: "history" | "stack") {
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

const byLabel = (label: string) => container.querySelectorAll(`[aria-label="${label}"]`);
const buttonByText = (text: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(text));

describe("粘贴堆栈端到端（T-B3-3）", () => {
  it("stackSection_pushFromRowMenu_invokes：历史行「入栈」→ clipboard_stack_push 带条目 id", async () => {
    await mount("history");
    await click(byLabel("入栈")[0]);
    expect(clipboardStackPush).toHaveBeenCalledWith("h1");
    expect(notify).toHaveBeenCalledWith("success", "已加入粘贴堆栈", "当前队列 1 条");
  });

  it("stackSection_pasteNext_hidesWindowBeforeInvokeAndRestores：先隐藏让焦点 → 投递 → 恢复", async () => {
    await mount("stack");
    expect(clipboardStackList).toHaveBeenCalled();
    expect(container.textContent).toContain("预览-s1");
    await click(buttonByText("粘贴下一条"));
    expect(seq).toEqual(["hide", "paste_next", "show", "setFocus"]);
    expect(win.hide).toHaveBeenCalledTimes(1);
    expect(win.show).toHaveBeenCalledTimes(1);
    expect(notify).toHaveBeenCalledWith("success", "已粘贴 1 条", "该项已出栈");
  });

  it("stackSection_pasteAll_sendsIntervalMsFromNumberField：间隔数值框的值即 paste_all 入参", async () => {
    await mount("stack");
    const input = byLabel("粘贴间隔(ms)")[0] as HTMLInputElement;
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    await act(async () => {
      setter!.call(input, "1200");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(buttonByText("全部粘贴"));
    expect(clipboardStackPasteAll).toHaveBeenCalledWith(1200);
    expect(container.textContent).toContain("队列 2 条");
  });

  it("stackSection_moveAndRemove_invokeShapes：↑↓ 传目标下标、移出只点本行", async () => {
    await mount("stack");
    expect(byLabel("后移")).toHaveLength(2);
    await click(byLabel("后移")[0]);
    expect(clipboardStackMove).toHaveBeenCalledWith("s1", 1);
    await click(byLabel("移出")[1]);
    expect(clipboardStackRemove).toHaveBeenCalledWith("s2");
  });

  it("stackSection_clear_dangerConfirmCancelZeroInvoke：确认框取消即零调用", async () => {
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await mount("stack");
    await click(buttonByText("清空"));
    expect(confirmAction).toHaveBeenCalledWith(expect.objectContaining({ title: "清空粘贴堆栈" }));
    expect(clipboardStackClear).not.toHaveBeenCalled();
    expect(container.textContent).toContain("预览-s1");

    await click(buttonByText("清空"));
    expect(clipboardStackClear).toHaveBeenCalledTimes(1);
  });
});

describe("堆栈投递诚实面", () => {
  it("stackSection_pasteNext_secretHead_reportsRefusalAndKeepsQueue", async () => {
    vi.mocked(clipboardStackPasteNext).mockResolvedValueOnce({
      id: "s1",
      delivered: false,
      error: "敏感条目需先揭示后粘贴",
    });
    await mount("stack");
    await click(buttonByText("粘贴下一条"));
    expect(notify).toHaveBeenCalledWith(
      "error",
      "未粘贴，该项仍在栈上",
      "敏感条目需先揭示后粘贴",
    );
    expect(clipboardStackList).toHaveBeenCalledTimes(2);
  });

  it("stackSection_empty_hidesDeliveryButtonsNoInvoke", async () => {
    vi.mocked(clipboardStackList).mockResolvedValue([]);
    await mount("stack");
    expect(container.textContent).toContain("堆栈为空");
    expect((buttonByText("粘贴下一条") as HTMLButtonElement).disabled).toBe(true);
    expect(clipboardStackPasteNext).not.toHaveBeenCalled();
  });
});
