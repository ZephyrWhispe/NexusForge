import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardClear,
  clipboardCaptureGet,
  clipboardCaptureSet,
  clipboardGet,
  clipboardGroupCounts,
  clipboardGroupDelete,
  clipboardGroupRename,
  clipboardEntrySetGroup,
  clipboardStats,
  clipboardSuggestionApply,
  clipboardSuggestions,
  clipboardSearch,
  clipboardStackList,
  hostConfigGet,
  hostConfigSchema,
  type ClipEntry,
} from "../../../ipc/client";
import { useSession, type ClipView } from "../../../stores/session";
import { SUBNAV } from "../../../layout/modules";

// D-29 B3/T-B3-1 回归（09 §8.2）：剪切板五子面板各自挂载、各发自己那份 invoke；
// 堆栈/敏感两区在新命令面落地前必须是零调用的诚实空态（宁可空，不拿假列表冒充功能）。

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
    clipboardSuggestions: vi.fn(),
    clipboardSuggestionApply: vi.fn(),
    clipboardGroupRename: vi.fn(),
    clipboardGroupDelete: vi.fn(),
    clipboardEntrySetGroup: vi.fn(),
    clipboardStats: vi.fn(),
    clipboardCaptureGet: vi.fn(),
    clipboardCaptureSet: vi.fn(),
    clipboardStackList: vi.fn(),
    hostConfigSchema: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
// jsdom 无 Tauri 事件后端：listen() 必然 reject，本用例只关心「谁被调用」，故给直通假卸载
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

// jsdom 无 ResizeObserver/真实 rect，虚拟器观测不到滚动容器就不产行（同 toolbarDetail.test 处置）
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
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("c1")],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardGroupCounts).mockResolvedValue({ text: 3, code: 1 });
  // T-B3-4：分组视图挂载即拉建议，设置视图挂载即拉统计（不 mock 会打到真 invoke）
  vi.mocked(clipboardSuggestions).mockResolvedValue([]);
  vi.mocked(clipboardSuggestionApply).mockResolvedValue(0);
  vi.mocked(clipboardGroupRename).mockResolvedValue(0);
  vi.mocked(clipboardGroupDelete).mockResolvedValue(0);
  vi.mocked(clipboardEntrySetGroup).mockResolvedValue(undefined);
  vi.mocked(clipboardStats).mockResolvedValue({
    total: 4,
    by_content_type: { text: 4 },
    by_group: { 未分组: 4 },
    top_source_apps: [],
    bytes_blob: 128,
  });
  vi.mocked(clipboardGet).mockResolvedValue("");
  vi.mocked(clipboardClear).mockResolvedValue(0);
  vi.mocked(hostConfigSchema).mockResolvedValue({
    properties: {
      max_entries: { type: "integer", title: "保留条数", minimum: 1, maximum: 9999 },
      // T-B3-2：schema 里带 readOnly 的键由专用卡承载，通用表单不得再出一个写口
      capture_paused: { type: "boolean", title: "暂停捕获", default: false, readOnly: true },
    },
  });
  vi.mocked(hostConfigGet).mockResolvedValue({});
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardCaptureSet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardStackList).mockResolvedValue([]);
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

async function mountView(view: ClipView) {
  // 真实用户路径：SubNav 点「视图」→ session 键 clipView → 面板壳读取并分流
  act(() => useSession.getState().setClipView(view));
  await act(async () => {
    root = createRoot(container);
    root.render(<ClipboardPanel search="" group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
}

describe("剪切板五子面板挂载（T-B3-1）", () => {
  it("clipPanel_fiveSectionsEachMountsWithOwnInvoke：五区逐个挂载，未落地命令面的两区零 invoke", async () => {
    // 注册表 ↔ store 联合一致性：SUBNAV 的 view 项 id 必须都在 ClipView 联合内
    const viewIds = SUBNAV.clipboard
      .flatMap((s) => s.items)
      .filter((i) => (i.scope ?? "filter") === "view")
      .map((i) => i.id);
    expect(viewIds).toEqual(["history", "groups", "stack", "secret", "settings"]);

    await mountView("history");
    expect(clipboardSearch).toHaveBeenCalled();
    expect(container.textContent).toContain("短预览");
    act(() => {
      root?.unmount();
    });
    root = null;
    vi.clearAllMocks();

    await mountView("groups");
    expect(clipboardGroupCounts).toHaveBeenCalled();
    expect(container.textContent).toContain("分组");
    expect(clipboardSearch).not.toHaveBeenCalled();
    act(() => {
      root?.unmount();
    });
    root = null;
    vi.clearAllMocks();

    await mountView("settings");
    expect(hostConfigSchema).toHaveBeenCalledWith("clipboard");
    expect(container.textContent).toContain("保留条数");
    act(() => {
      root?.unmount();
    });
    root = null;
    vi.clearAllMocks();

    // T-B3-3 起堆栈区有真命令面（clipboard_stack_list），故此处只剩敏感区仍是零调用的诚实空态
    await mountView("stack");
    expect(clipboardStackList).toHaveBeenCalledTimes(1);
    expect(clipboardSearch, "stack 不应查历史").not.toHaveBeenCalled();
    expect(clipboardGroupCounts, "stack 不应读分组计数").not.toHaveBeenCalled();
    expect(hostConfigSchema, "stack 不应读设置").not.toHaveBeenCalled();
    act(() => {
      root?.unmount();
    });
    root = null;
    vi.clearAllMocks();

    await mountView("secret");
    expect(clipboardSearch, "secret 不应查历史").not.toHaveBeenCalled();
    expect(clipboardGroupCounts, "secret 不应读分组计数").not.toHaveBeenCalled();
    expect(hostConfigSchema, "secret 不应读设置").not.toHaveBeenCalled();
    expect(clipboardGet).not.toHaveBeenCalled();
    expect(container.textContent).toContain("T-B3");
  });
});

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function remount(view: ClipView) {
  act(() => root?.unmount());
  root = null;
  vi.clearAllMocks();
  await mountView(view);
}

describe("暂停捕获（T-B3-2）", () => {
  it("captureSwitch_settingsSectionToggle_invokesAndReflectsPaused：开关即 clipboard_capture_set，显示返回的运行期真值", async () => {
    await mountView("settings");
    expect(clipboardCaptureGet).toHaveBeenCalledTimes(1);
    // 单一写口：schema 里的 readOnly capture_paused 不得再被通用表单渲染成第二个开关
    const boxes = [...container.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
    expect(boxes).toHaveLength(1);
    expect(boxes[0].checked).toBe(false);
    expect(container.textContent).toContain("运行中");
    // 正对照：通用表单照常渲染非 readOnly 键
    expect(container.textContent).toContain("保留条数");

    vi.mocked(clipboardCaptureSet).mockResolvedValue({ paused: true, skipped: 2 });
    await click(boxes[0]);
    expect(clipboardCaptureSet).toHaveBeenCalledWith(true);
    expect(container.textContent).toContain("已暂停");
    expect(container.textContent).toContain("暂停期间已跳过 2 次复制");
  });

  it("captureBanner_historySection_showsSkippedCountHonest：skipped>0 才出横幅，0 时零文案", async () => {
    vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: true, skipped: 5 });
    await mountView("history");
    expect(clipboardCaptureGet).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("暂停期间已跳过 5 次复制");

    vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: true, skipped: 0 });
    await remount("history");
    expect(container.textContent).not.toContain("暂停期间已跳过");
  });
});
