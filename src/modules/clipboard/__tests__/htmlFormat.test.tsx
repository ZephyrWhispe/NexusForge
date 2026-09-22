import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardGet,
  clipboardHtmlGet,
  clipboardPaste,
  clipboardSearch,
  type ClipEntry,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

// D-29 B3/T-B3-8 回归：HTML 正文只经 clipboard_html_get 显式读口出 IPC（列表带布尔），
// 详情框以文本节点渲染源文（进 innerHTML 就是自造 XSS 面），行操作把粘贴拆成
// 「纯文本 / 带格式」两钮，且带格式钮只在真带 HTML 的行上出现。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    clipboardSearch: vi.fn(),
    clipboardGet: vi.fn(),
    clipboardHtmlGet: vi.fn(),
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

// jsdom 无 ResizeObserver/真实 rect：直通假实现按 count 全量展开虚拟行（同其余面板测）。
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
    preview: "富文本一行",
    blob_path: null,
    origin: "local",
    source_app: "Chrome",
    pinned: false,
    group: null,
    secret: false,
    has_html: false,
    created_at: Date.parse("2026-09-22T07:05:00"),
    usage_count: 0,
    ...over,
  };
}

/** HTML 源文夹具：带标签与属性，一旦被当 markup 注入就会长出元素子节点 */
const SOURCE = '<p class="x">富文本一行<b>加粗</b></p>';

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("c1", { has_html: true })],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardGet).mockResolvedValue("富文本一行");
  vi.mocked(clipboardHtmlGet).mockResolvedValue(SOURCE);
  vi.mocked(clipboardPaste).mockResolvedValue({ format_used: "html", degraded: false });
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

function tab(label: string): HTMLElement | null {
  return (
    [...document.querySelectorAll<HTMLElement>('[role="tab"]')].find(
      (el) => el.textContent?.trim() === label,
    ) ?? null
  );
}

/**
 * src/ 全量源码（Vite `?raw` 内联，编译期成表：不引 @types/node、也不依赖测试进程的 cwd）。
 * 根绝对式 glob 让键形如 `/src/modules/xxx/Yyy.tsx`——相对式 glob 会把键压成
 * "最短相对路径"（同一模块内的文件连目录名都不带），路径判据就无从落脚。
 */
const srcBodies: [string, string][] = Object.entries(
  import.meta.glob("/src/**/*.{ts,tsx}", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
)
  .filter(([rel]) => !rel.endsWith("__tests__/htmlFormat.test.tsx"))
  .map(([rel, body]) => [rel.slice("/src/".length), body]);

/**
 * 本行开工前既有的 HTML 注入面（markdown 预览 ×2、xterm 容器清空 ×1）：不在 B3 范围，
 * 但也不能假装不存在。列成清单的含义是"只许少不许多"——任何新增宿主即判红。
 */
const PREEXISTING_SINKS = [
  "modules/editor/EditorPanel.tsx",
  "modules/notes/NotesPanel.tsx",
  "modules/term/TerminalPanel.tsx",
];

/** 扫描器自身已按路径排除（见上）：它若含那两个字面量就会永远命中自己，检查再也不会失效地"通过" */
const sinksIn = (body: string) =>
  body.includes("dangerouslySetInnerHTML") || /\.innerHTML\s*(\+?=[^=])/.test(body);

describe("剪贴板 HTML 捕获与格式粘贴（T-B3-8）", () => {
  it("detailDialog_htmlTab_readsViaHtmlGet_andNeverUsesInnerHtml", async () => {
    // 静态红线（任务书"全 src/ 零命中"，落地修正见 09 行落地补记）：
    // ① 剪贴板模块——本行交付的渲染面——严格零命中；
    // ② 全 src/ 命中集须恰等于上面那份既有清单，即"不许再多一枚宿主"（兼作 ① 的正对照：
    //    它证明扫描确实看得见命中，而不是永远返回空集的哑检查）。
    expect(srcBodies.length, "src/ 源码须扫到足量文件，接近空集等于检查空转").toBeGreaterThan(80);
    const hits = srcBodies.filter(([, body]) => sinksIn(body)).map(([rel]) => rel);
    expect(hits.filter((rel) => rel.startsWith("modules/clipboard/"))).toEqual([]);
    expect(hits.sort()).toEqual([...PREEXISTING_SINKS].sort());

    await mount();
    await click(container.querySelector('[aria-label="详情"]')!);
    // 默认视图不提前拉正文：列表只带布尔，源文最多 512KB，不随开框就拖出 IPC
    expect(clipboardHtmlGet).not.toHaveBeenCalled();

    await click(tab("HTML 源")!);
    expect(clipboardHtmlGet).toHaveBeenCalledWith("c1");
    const pre = document.querySelector("pre");
    expect(pre?.textContent).toBe(SOURCE);
    // 若被当 markup 注入，这里会长出 <b>/<p> 元素子节点；文本节点渲染则恒 0
    expect(pre?.children.length).toBe(0);

    // 切回纯文本不重复拉（缓存生效），再切回 HTML 也仍是一次
    await click(tab("纯文本")!);
    await click(tab("HTML 源")!);
    expect(clipboardHtmlGet).toHaveBeenCalledTimes(1);
  });

  it("pasteMenu_pasteAsHtml_sendsFormatHtml", async () => {
    await mount();
    const btn = container.querySelector('[aria-label="粘贴带格式"]');
    expect(btn).not.toBeNull();
    await click(btn!);
    expect(clipboardPaste).toHaveBeenCalledWith("c1", "html");
    expect(notify).toHaveBeenCalledWith(
      "success",
      "已写入剪贴板（含 HTML）",
      expect.any(String),
    );
  });

  it("pasteMenu_degradedResult_warnsInsteadOfSilentSuccess", async () => {
    vi.mocked(clipboardPaste).mockResolvedValue({ format_used: "plain", degraded: true });
    await mount();
    await click(container.querySelector('[aria-label="粘贴带格式"]')!);
    expect(notify).toHaveBeenCalledWith("warn", "已按纯文本粘贴", expect.stringContaining("没有 HTML 正文"));
    expect(notify).not.toHaveBeenCalledWith("success", expect.anything(), expect.anything());
  });

  it("pasteRow_plainWhenNoHtml_badgeAbsent", async () => {
    // 同一张表里两行：一有格式一没有——缺席断言必须有在场正对照，否则只是"整个操作列没渲染"
    vi.mocked(clipboardSearch).mockResolvedValue({
      items: [clip("c1", { has_html: false }), clip("c2", { has_html: true })],
      has_more: false,
      total: null,
    });
    await mount();
    expect(container.querySelectorAll('[aria-label="粘贴带格式"]').length).toBe(1);
    expect(container.querySelectorAll('[aria-label="粘贴为纯文本"]').length).toBe(2);

    await click(container.querySelector('[aria-label="粘贴为纯文本"]')!);
    expect(clipboardPaste).toHaveBeenCalledWith("c1", "plain");
    expect(clipboardHtmlGet).not.toHaveBeenCalled();
  });
});
