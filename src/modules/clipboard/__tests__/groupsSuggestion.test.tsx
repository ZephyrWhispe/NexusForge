import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardCaptureGet,
  clipboardEntrySetGroup,
  clipboardGet,
  clipboardGroupCounts,
  clipboardGroupDelete,
  clipboardGroupRename,
  clipboardSearch,
  clipboardStats,
  clipboardSuggestionApply,
  clipboardSuggestions,
  hostConfigGet,
  hostConfigSchema,
  type ClipEntry,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";
import { useSession } from "../../../stores/session";

// D-29 B3/T-B3-4 回归（09 §8.2）：分组树写口 + 建议卡墙采纳/忽略 + 统计卡。
// 建议制红线：忽略只静默建议，绝不改分组；统计卡一律读 clipboard_stats 的库内聚合。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    clipboardSearch: vi.fn(),
    clipboardGet: vi.fn(),
    clipboardGroupCounts: vi.fn(),
    clipboardCaptureGet: vi.fn(),
    clipboardSuggestions: vi.fn(),
    clipboardSuggestionApply: vi.fn(),
    clipboardGroupRename: vi.fn(),
    clipboardGroupDelete: vi.fn(),
    clipboardEntrySetGroup: vi.fn(),
    clipboardStats: vi.fn(),
    hostConfigSchema: vi.fn(),
    hostConfigGet: vi.fn(),
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
    created_at: Date.parse("2026-09-22T07:05:00"),
    usage_count: 0,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root | null = null;

/** 建议队列夹具：可变，apply 后真从队列移出——卡片消失是库口径，不是前端自说自话 */
let suggFixture: {
  entry_id: string;
  preview: string;
  suggested_group: string;
  confidence: number;
}[] = [];

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  suggFixture = [
    { entry_id: "s1", preview: "https://example.com/x", suggested_group: "url", confidence: 0.95 },
    { entry_id: "s2", preview: "{\"a\":1}", suggested_group: "json", confidence: 0.9 },
  ];
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("h1", { group: "工作 项目" })],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardGet).mockResolvedValue("全文");
  vi.mocked(clipboardGroupCounts).mockResolvedValue({
    all: 5,
    text: 2,
    secret: 1,
    "工作 项目": 2,
    代码: 1,
  });
  vi.mocked(clipboardSuggestions).mockImplementation(async () => suggFixture);
  vi.mocked(clipboardSuggestionApply).mockImplementation(async (ids, accept) => {
    const before = suggFixture.length;
    suggFixture = suggFixture.filter((s) => !ids.includes(s.entry_id));
    void accept;
    return before - suggFixture.length;
  });
  vi.mocked(clipboardGroupRename).mockResolvedValue(2);
  vi.mocked(clipboardGroupDelete).mockResolvedValue(2);
  vi.mocked(clipboardEntrySetGroup).mockResolvedValue(undefined);
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardStats).mockResolvedValue({
    total: 5,
    by_content_type: { text: 4, files: 1 },
    by_group: { 未分组: 2, "工作 项目": 2, 代码: 1 },
    top_source_apps: [["Notepad", 3]],
    bytes_blob: 2048,
  });
  vi.mocked(hostConfigSchema).mockResolvedValue({ properties: {} });
  vi.mocked(hostConfigGet).mockResolvedValue({});
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

async function mount(view: "groups" | "settings") {
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
  [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(text));

/** 原生 setter 赋值：React 受控 Input 只认这条路径 */
async function typeInto(label: string, value: string) {
  const input = container.querySelector(`[aria-label="${label}"]`) as HTMLInputElement | null;
  if (!input) throw new Error(`输入框 ${label} 未渲染`);
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("分组树与建议卡墙（T-B3-4）", () => {
  it("groupsSection_treeRenameDelete_invokes", async () => {
    await mount("groups");
    // 计数对象按真名渲染（含空格组名），内置桶标注为筛选不可改
    expect(container.textContent).toContain("工作 项目");
    expect(container.textContent).toContain("内置筛选");

    await click(buttonByText("重命名"));
    await typeInto("新组名", "远程 团队");
    await click(buttonByText("确定"));
    expect(clipboardGroupRename).toHaveBeenCalledWith("工作 项目", "远程 团队");

    await click(buttonByText("删除"));
    expect(confirmAction).toHaveBeenCalledWith(
      expect.objectContaining({ title: "删除分组「工作 项目」" }),
    );
    expect(clipboardGroupDelete).toHaveBeenCalledWith("工作 项目");
    expect(notify).toHaveBeenCalledWith("success", "分组已删除", "2 条条目改为未分组");
  });

  it("suggestionCard_accept_appliesAndRemovesCard", async () => {
    await mount("groups");
    expect(container.textContent).toContain("https://example.com/x");
    expect(container.textContent).toContain("建议归入「链接」");

    await click(buttonByText("采纳"));
    expect(clipboardSuggestionApply).toHaveBeenCalledWith(["s1"], true);
    // 本地即时摘卡 + 重新拉队列（真源仍是库，不以乐观更新冒充）
    expect(container.textContent).not.toContain("https://example.com/x");
    expect(clipboardSuggestions).toHaveBeenCalledTimes(2);
    expect(clipboardGroupRename, "采纳走 suggestion_apply，不走整组重命名").not.toHaveBeenCalled();
  });

  it("suggestionCard_ignore_dismissesWithoutGroupChange", async () => {
    await mount("groups");
    await click(buttonByText("忽略"));
    expect(clipboardSuggestionApply).toHaveBeenCalledWith(["s1"], false);
    expect(clipboardGroupRename).not.toHaveBeenCalled();
    expect(clipboardGroupDelete).not.toHaveBeenCalled();
    expect(clipboardEntrySetGroup, "忽略绝不顺手改分组").not.toHaveBeenCalled();
    expect(container.textContent).not.toContain("https://example.com/x");
  });

  it("statsSection_rendersFromClipboardStats", async () => {
    await mount("settings");
    expect(clipboardStats).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("共 5 条");
    expect(container.textContent).toContain("2.0 KB");
    expect(container.textContent).toContain("工作 项目 2");
    expect(container.textContent).toContain("Notepad 3");
  });
});
