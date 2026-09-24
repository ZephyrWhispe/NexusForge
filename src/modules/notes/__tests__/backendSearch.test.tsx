import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel from "../NotesPanel";
import {
  notesCanvasDirs,
  notesCards,
  notesList,
  notesReviewQueue,
  notesSearch,
  notesSync,
  type NoteMetaDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-21：搜索框从内存 includes 换后端 FTS5。
// 判据：①query 变化（防抖后）触发 notesSearch invoke；②仅正文命中的行出现在 DOM——
// 旧内存过滤路径（path/title/tags includes）下该行不可能在场，即旧闭包零消费者。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesCards: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesSync: vi.fn(),
    notesSearch: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const META: NoteMetaDto = {
  path: "ops/log.md",
  title: "运维记",
  tags: [],
  mtime_ms: 1700000000000,
  size: 42,
};

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

function searchInput(): HTMLInputElement {
  const el = [...container.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith("搜索"),
  ) as HTMLInputElement;
  return el;
}

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function flush() {
  await act(async () => {});
}

async function settle(ms: number) {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(notesList).mockResolvedValue([META]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesCanvasDirs).mockResolvedValue([]);
  vi.mocked(notesSync).mockResolvedValue({ added: 0, updated: 0, removed: 0, total: 1 });
  vi.mocked(notesSearch).mockResolvedValue([
    { path: "ops/log.md", title: "运维记", snippet: "今天同步失败 重试成功", rank: -1.5 },
  ]);
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

describe("NotesPanel 后端搜索（T-B7-21）", () => {
  it("notesPanel_searchGoesBackend：query 变化触发 invoke，仅正文命中行经后端出现在 DOM", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<NotesPanel />);
    });
    await flush();
    expect(notesSearch).not.toHaveBeenCalled();

    await setInput(searchInput(), "同步失败");
    await settle(300); // 越过 200ms 防抖
    expect(notesSearch).toHaveBeenCalledWith("同步失败", 200);

    // 正文命中分组在场（旧内存 includes 下该 query 对 path/title/tags 全不命中→不可能渲染）
    expect(container.textContent).toContain("正文命中（1）");
    expect(container.textContent).toContain("今天同步失败 重试成功");
    expect(container.querySelectorAll(`[role="button"]`).length).toBeGreaterThan(0);

    // 清空 → 退出搜索态，回到全量列表（零 invoke 新增）
    const calls = vi.mocked(notesSearch).mock.calls.length;
    await setInput(searchInput(), "");
    await flush();
    expect(vi.mocked(notesSearch).mock.calls.length).toBe(calls);
    expect(container.textContent).toContain("运维记");
    expect(container.textContent).not.toContain("正文命中");
    expect(buttonByText("重建索引")).toBeDefined();
  });
});
