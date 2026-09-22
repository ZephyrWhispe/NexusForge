import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import { clipSearchParams, mirrorClipSearchSyntax } from "../panels/HistorySection";
import {
  clipboardCaptureGet,
  clipboardClear,
  clipboardGet,
  clipboardGroupCounts,
  clipboardSearch,
} from "../../../ipc/client";

// T-B3-6（09 §8.2）搜索语法前端面：group:/type: 两枚语法既要不丢后端的 AND 语义，
// 又要在用户接管（点类型芯片 / 清分组芯片）时把对应 token 从下传文本里剥出去。
// 判定权威只有一份（query.rs），本文件只测"镜像后的 payload 长什么样"。

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
    clipboardEntrySetGroup: vi.fn(),
    clipboardStackPush: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

// jsdom 无 ResizeObserver/真实 rect：直通假实现按 count 全量展开，行才渲染得出来
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 64,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({ index: i, start: i * 64, key: i, size: 64 })),
    measureElement: () => {},
  }),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(clipboardSearch).mockResolvedValue({ items: [], has_more: false, total: null });
  vi.mocked(clipboardGroupCounts).mockResolvedValue({});
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
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

async function mount(search: string, group = "all") {
  await act(async () => {
    root = createRoot(container);
    root.render(<ClipboardPanel search={search} group={group} onCounts={() => {}} />);
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}
function lastQuery() {
  const calls = vi.mocked(clipboardSearch).mock.calls;
  return calls[calls.length - 1][0];
}

describe("搜索语法前端面（T-B3-6）", () => {
  it("searchBox_typeChip_setsContentTypeAndReinvokes：点芯片即带 content_type 重查一次（再点收回）", async () => {
    await mount("");
    // 挂载首轮无类型条件（"all" 分组按既有四测不进 payload，故此处 group 为 undefined）
    expect(lastQuery()).toEqual({
      text: undefined,
      group: undefined,
      page: 0,
      size: 50,
      content_type: undefined,
    });

    await click(buttonByText("图片")!);
    expect(clipboardSearch).toHaveBeenCalledTimes(2);
    expect(lastQuery()).toEqual({
      text: undefined,
      group: undefined,
      page: 0,
      size: 50,
      content_type: "image",
    });

    // 单选 toggle：再点同一枚 = 收回条件，而不是叠成 image AND image
    await click(buttonByText("图片")!);
    expect(lastQuery().content_type).toBeUndefined();
    // 正对照：点另一枚是换档不是清空
    await click(buttonByText("文件")!);
    expect(lastQuery().content_type).toBe("files");
  });

  it("searchSyntax_groupChipVisible_andClearRestoresFullList：只读组芯片显示语法组，点一下回到未加法语筛选", async () => {
    await mount("group:工作 报告");
    expect(lastQuery()).toEqual({
      text: "报告",
      group: "工作",
      page: 0,
      size: 50,
      content_type: undefined,
    });
    const chip = container.querySelector('[aria-label="清除分组筛选"]');
    expect(chip).not.toBeNull();
    expect(chip?.textContent).toContain("组：工作");

    await click(chip!);
    expect(clipboardSearch).toHaveBeenCalledTimes(2);
    expect(lastQuery()).toEqual({
      text: "报告",
      group: undefined,
      page: 0,
      size: 50,
      content_type: undefined,
    });
    // 清掉后芯片即消失（不留一个"点了没反应"的幽灵控件）
    expect(container.querySelector('[aria-label="清除分组筛选"]')).toBeNull();
  });

  it("searchSyntax_unknownTypeToken_staysInTextAndSaysSo：未知 type 原样下传交后端滤空，前端不另判", async () => {
    await mount("type:pdf 报告");
    expect(lastQuery().text).toBe("type:pdf 报告");
    expect(lastQuery().content_type).toBeUndefined();
    expect(container.textContent).toContain("按零命中处理");
    expect(container.textContent).toContain("pdf");
  });
});

describe("语法镜像纯函数（query.rs 的前端镜像）", () => {
  it("mirror 与后端的三档/引号/URL 三处判定同形", () => {
    expect(mirrorClipSearchSyntax('group:"工作 笔记" 评审')).toMatchObject({
      text: "评审",
      group: "工作 笔记",
    });
    // URL 守卫：http:// 里也有冒号，但它不是语法
    expect(mirrorClipSearchSyntax("http://a:b/c")).toMatchObject({
      text: "http://a:b/c",
      group: undefined,
      type: undefined,
    });
    expect(mirrorClipSearchSyntax("type:files").type).toBe("files");
    expect(mirrorClipSearchSyntax("type:pdf").unknownType).toBe(true);
    // 重复前缀后现覆盖先前（与 Rust 同名测试同语义）
    expect(mirrorClipSearchSyntax("group:甲 group:乙").group).toBe("乙");
  });

  it("clipSearchParams 未知 type 残渣保留在 text，已知 token 剥出 text", () => {
    expect(clipSearchParams("type:pdf x", "all", 0).text).toBe("type:pdf x");
    expect(clipSearchParams("type:image x", "all", 0)).toMatchObject({
      text: "x",
      content_type: "image",
    });
    // 芯片值优先于语法（用户接管即以后者为准），但只在调用方把 token 剥掉后才成立
    expect(clipSearchParams("type:image x", "all", 0, 50, "files").content_type).toBe("files");
    const stripped = mirrorClipSearchSyntax("type:image x");
    expect(clipSearchParams(stripped.text, "all", 0, 50, undefined).content_type).toBeUndefined();
  });
});
