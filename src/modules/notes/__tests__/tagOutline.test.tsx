import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel, { extractHeadings } from "../NotesPanel";
import {
  notesBacklinks,
  notesByTag,
  notesCanvasDirs,
  notesCards,
  notesLinks,
  notesList,
  notesRead,
  notesReviewQueue,
  notesSync,
  type NoteMetaDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-22：标签后端查询（芯片点选=notes_by_tag invoke）+ 前端纯函数大纲
// （extractHeadings：ATX/Setext/围栏排除）+ 大纲点击定位编辑光标。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesCards: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesSync: vi.fn(),
    notesByTag: vi.fn(),
    notesRead: vi.fn(),
    notesLinks: vi.fn(),
    notesBacklinks: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

// T-B7-23：编辑核换 Monaco——形制桩镜像 autosaveDiscipline（FakeModel 同步派发
// setValue 变更、isDisposed 后 getValue 抛错），定位断言从 textarea selectionStart
// 改为Fake editor 记录的 setPosition。
const stub = vi.hoisted(() => ({
  createCount: 0,
  editors: [] as Array<{
    handlers: Array<() => void>;
    current: { getValue: () => string; setValue: (v: string) => void; isDisposed: () => boolean } | null;
    positionCalls: Array<{ lineNumber: number; column: number }>;
    revealed: number[];
  }>,
}));

vi.mock("../../../monaco/setup", () => {
  class FakeModel {
    private value = "";
    private disposed = false;
    constructor(private fire: () => void) {}
    isDisposed() {
      return this.disposed;
    }
    setValue(v: string) {
      this.value = v;
      this.fire(); // monaco 语义：变更监听器同步派发
    }
    getValue() {
      if (this.disposed) throw new Error("Model is disposed!");
      return this.value;
    }
    getLanguageId() {
      return "markdown";
    }
    onDidChangeContent() {
      return { dispose() {} };
    }
    dispose() {
      this.disposed = true;
    }
  }
  return {
    languageForPath: () => "markdown",
    monaco: {
      editor: {
        create: () => {
          stub.createCount += 1;
          const editor = {
            handlers: [] as Array<() => void>,
            current: null as FakeModel | null,
            positionCalls: [] as Array<{ lineNumber: number; column: number }>,
            revealed: [] as number[],
          };
          stub.editors.push(editor as unknown as (typeof stub.editors)[number]);
          return {
            onDidChangeModelContent: (h: () => void) => {
              editor.handlers.push(h);
              return { dispose() {} };
            },
            getModel: () => editor.current,
            setModel: (m: FakeModel) => {
              editor.current = m;
            },
            updateOptions: () => {},
            revealLineInCenterIfOutsideViewport: (line: number) => {
              editor.revealed.push(line);
            },
            setPosition: (p: { lineNumber: number; column: number }) => {
              editor.positionCalls.push(p);
            },
            focus: () => {},
            dispose: () => {},
          };
        },
        createModel: () => {
          const editor = stub.editors[stub.editors.length - 1];
          return new FakeModel(() => editor.handlers.forEach((h) => h()));
        },
        setModelLanguage: () => {},
      },
      languages: {
        registerCompletionItemProvider: () => ({ dispose() {} }),
        CompletionItemKind: { Reference: 17 },
      },
    },
  };
});

const A: NoteMetaDto = {
  path: "a.md",
  title: "甲笔记",
  tags: ["work"],
  mtime_ms: 1700000000000,
  size: 10,
};
const B: NoteMetaDto = {
  path: "b.md",
  title: "乙笔记",
  tags: ["workshop"],
  mtime_ms: 1700000000000,
  size: 10,
};

const DOC = "# 一级\n\n段落\n\n## 二级\n正文";

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function flush() {
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<NotesPanel />);
  });
  await flush();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  stub.createCount = 0;
  stub.editors.length = 0;
  vi.mocked(notesList).mockResolvedValue([A, B]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesCanvasDirs).mockResolvedValue([]);
  vi.mocked(notesSync).mockResolvedValue({ added: 0, updated: 0, removed: 0, total: 2 });
  vi.mocked(notesByTag).mockResolvedValue([A]);
  vi.mocked(notesRead).mockResolvedValue({ content: DOC, meta: A });
  vi.mocked(notesLinks).mockResolvedValue([]);
  vi.mocked(notesBacklinks).mockResolvedValue([]);
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

describe("extractHeadings 前端纯函数（T-B7-22）", () => {
  it("extractHeadings_setextAndFenced：三夹具 ATX/Setext/围栏内假标题", () => {
    // 夹具①：ATX 两级
    expect(extractHeadings("# 顶\n###### 最深\n正文")).toEqual([
      { level: 1, text: "顶", line: 1 },
      { level: 6, text: "最深", line: 2 },
    ]);
    // 夹具②：Setext 两式（=== 一级 / --- 二级），行号=标题文字行
    expect(extractHeadings("标题\n====\n\n副题\n---\n\n\n---")).toEqual([
      { level: 1, text: "标题", line: 1 },
      { level: 2, text: "副题", line: 4 },
    ]);
    // 夹具③：围栏内 # 不算 + frontmatter 跳过
    const fenced = [
      "---",
      "title: x",
      "---",
      "",
      "# 真标题",
      "```",
      "# 假标题",
      "~~~ 也假",
      "```",
      "## 回到真",
    ].join("\n");
    expect(extractHeadings(fenced)).toEqual([
      { level: 1, text: "真标题", line: 5 },
      { level: 2, text: "回到真", line: 10 },
    ]);
  });
});

describe("NotesPanel 标签芯片 + 大纲（T-B7-22）", () => {
  it("notesPanel_tagChipFilter_callsBackend：点芯片=notes_by_tag invoke，再点退出", async () => {
    await mount();
    expect(container.textContent).toContain("甲笔记");
    expect(container.textContent).toContain("乙笔记");

    await click(buttonByText("#work")!);
    expect(notesByTag).toHaveBeenCalledWith("work");
    await flush();
    // 后端结果替换基础列表（workshop 不在其中——精确等值由后端保证）
    expect(container.textContent).toContain("甲笔记");
    expect(container.textContent).not.toContain("乙笔记");
    expect(container.textContent).toContain("标签「work」：1 篇（后端精确查询）");

    await click(buttonByText("#work")!);
    expect(vi.mocked(notesByTag).mock.calls.length).toBe(1); // 退出零新 invoke
    await flush();
    expect(container.textContent).toContain("乙笔记");
  });

  it("notesPanel_outlineJump_selectsLine：点大纲项 → Monaco 编辑核光标落到该标题行", async () => {
    await mount();
    await click(container.querySelectorAll(`[role="button"]`)[0]); // 打开 a.md
    await flush();
    const outline = [...container.querySelectorAll(`[role="button"]`)].filter((el) =>
      el.textContent?.trim() === "二级",
    );
    expect(outline.length).toBe(1);
    const ed = stub.editors[stub.editors.length - 1];
    await click(outline[0]);
    // DOC="# 一级\n\n段落\n\n## 二级\n正文" —— "## 二级" 在第 5 行
    expect(ed.positionCalls).toEqual([{ lineNumber: 5, column: 1 }]);
    expect(ed.revealed).toEqual([5]);
  });
});
