import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel from "../NotesPanel";
import {
  notesBacklinks,
  notesCanvasDirs,
  notesCards,
  notesList,
  notesLinks,
  notesRead,
  notesReviewQueue,
  notesSync,
  type NoteMetaDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-23：NotesPanel textarea → Monaco markdown + [[双链]] 补全。
// B1 教训镜像：程序化 setValue 不置脏（loadingLoadRef 闸）、编辑器只建一次
// （deps=存在性布尔）、切篇共享单 model 从不 dispose（"Model is disposed!" 回归钉）。

const stub = vi.hoisted(() => ({
  createCount: 0,
  disposedGetValueThrows: 0,
  providers: [] as Array<{
    triggerCharacters: string[];
    provideCompletionItems: (
      model: { getValueInRange: (r: { startLineNumber: number; endColumn: number }) => string },
      position: { lineNumber: number; column: number },
    ) => { suggestions: Array<{ insertText: string; range: unknown; label: string }> };
  }>,
  editors: [] as Array<{
    handlers: Array<() => void>;
    current: {
      getValue: () => string;
      setValue: (v: string) => void;
      isDisposed: () => boolean;
      getValueInRange: (r: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }) => string;
    } | null;
    positionCalls: Array<{ lineNumber: number; column: number }>;
    setModelCalls: number;
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
      if (this.disposed) {
        stub.disposedGetValueThrows += 1; // "Model is disposed!" 回归钉计数
        throw new Error("Model is disposed!");
      }
      return this.value;
    }
    getValueInRange(r: {
      startLineNumber: number;
      startColumn: number;
      endLineNumber: number;
      endColumn: number;
    }) {
      // 测试只用单行范围（补全按行取光标前文本）
      const line = this.value.split("\n")[r.startLineNumber - 1] ?? "";
      return line.slice(r.startColumn - 1, r.endColumn - 1);
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
            setModelCalls: 0,
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
              editor.setModelCalls += 1;
            },
            updateOptions: () => {},
            revealLineInCenterIfOutsideViewport: () => {},
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
        registerCompletionItemProvider: (_lang: string, provider: (typeof stub.providers)[number]) => {
          stub.providers.push(provider);
          return { dispose() {} };
        },
        CompletionItemKind: { Reference: 17 },
      },
    },
  };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesCards: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesSync: vi.fn(),
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

const A: NoteMetaDto = {
  path: "a.md",
  title: "甲笔记",
  tags: [],
  mtime_ms: 1700000000000,
  size: 10,
};
const C: NoteMetaDto = {
  path: "c.md",
  title: "测试稿",
  tags: [],
  mtime_ms: 1700000000000,
  size: 10,
};

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

async function openNote(path: "a.md" | "c.md") {
  const item = [...container.querySelectorAll(`[role="button"]`)].find((el) =>
    el.textContent?.includes(path),
  );
  expect(item).toBeDefined();
  await click(item!);
  await flush();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  stub.createCount = 0;
  stub.disposedGetValueThrows = 0;
  // stub.providers 故意不重置——registerWikiCompletion 模块级 once 闸在跨用例后仍闭合，
  // 恰成「重复挂载不重复注册」的活体负例
  stub.editors.length = 0;
  vi.mocked(notesList).mockResolvedValue([A, C]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesCanvasDirs).mockResolvedValue([]);
  vi.mocked(notesSync).mockResolvedValue({ added: 0, updated: 0, removed: 0, total: 2 });
  vi.mocked(notesRead).mockImplementation(async (p) => ({
    content: p === "a.md" ? "# 甲正文\n\n段落" : "# 测试正文\n\n段落",
    meta: p === "a.md" ? A : C,
  }));
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

describe("NotesPanel Monaco 换档纪律（T-B7-23）", () => {
  it("notesMonaco_createEffectDeps_isExistenceBoolean：建一次——双篇切换+脏标记+预览往返零重建", async () => {
    await mount();
    expect(stub.createCount).toBe(0); // 无活跃笔记不建编辑器
    await openNote("a.md");
    expect(stub.createCount).toBe(1);
    await openNote("c.md"); // 切篇
    const ed = stub.editors[0];
    // 用户态打字置脏（变更监听器直调 = monaco 派发同款语义）
    ed.current!.setValue("# 测试正文\n改一字");
    await flush();
    expect(container.textContent).toContain("未保存");
    await click(buttonByText("预览")!);
    await flush();
    await click(buttonByText("编辑")!);
    await flush();
    await openNote("a.md");
    expect(stub.createCount).toBe(1); // 全程恰一次
    expect(stub.editors.length).toBe(1);
  });

  it("notesMonaco_reuseKeepsDirtySemantics：openNote 程序化 setValue 不置脏（B1 误置脏教训镜像）", async () => {
    await mount();
    await openNote("a.md");
    // 换篇载入后不得出现「未保存」徽标（初版 textarea onChange 语义下 setValue 会误置脏）
    expect(container.textContent).not.toContain("未保存");
    await openNote("c.md");
    expect(container.textContent).not.toContain("未保存");
    // 真用户编辑照常置脏
    stub.editors[0].current!.setValue("# 测试正文\n手动编辑");
    await flush();
    expect(container.textContent).toContain("未保存");
  });

  it("notesMonaco_modelDisposedGuard_sessionSwitchNoErrors：切篇 10 次共享单 model 零 dispose 零抛错", async () => {
    await mount();
    await openNote("a.md");
    const ed = stub.editors[0];
    for (let i = 0; i < 5; i++) {
      await openNote("c.md");
      await openNote("a.md");
    }
    expect(ed.setModelCalls).toBe(1); // 单 model 常驻绑一次
    expect(ed.current!.isDisposed()).toBe(false);
    // "Model is disposed!" 回归钉：脏检查路径（change 监听器 isDisposed 早退）在场，
    // 切篇全程 getValue 通道无一抛错
    expect(stub.disposedGetValueThrows).toBe(0);
    expect(ed.current!.getValue()).toContain("甲正文");
  });

  it("notesPanel_previewToggle_stillWorksAfterMonaco：预览正对照——marked 通路零变，编辑器仅隐藏不卸载", async () => {
    await mount();
    await openNote("a.md");
    await click(buttonByText("预览")!);
    await flush();
    const previewDiv = [...container.querySelectorAll("div")].find((d) =>
      d.innerHTML.startsWith("<h1"),
    );
    expect(previewDiv, "预览态仍出 h1（marked 渲染通路未变）").toBeDefined();
    expect(previewDiv!.innerHTML).toContain("甲正文");
    expect(stub.createCount).toBe(1); // 预览切换不重建编辑器
    await click(buttonByText("编辑")!);
    await flush();
    expect(stub.editors[0].current!.getValue()).toContain("甲正文");
  });
});

describe("[[双链]] 补全 provider（T-B7-23）", () => {
  async function openAndCaptureProvider() {
    await mount();
    await openNote("a.md");
    // once 闸下全文件恰一枚 provider（重复挂载不重复注册）
    expect(stub.providers.length).toBe(1);
    return stub.providers[0];
  }

  it("wikiCompletion_listsExistingTitles：notes_list 两题缓存，[[ 触发候选集恰等", async () => {
    const provider = await openAndCaptureProvider();
    expect(provider.triggerCharacters).toEqual(["["]);
    const model = stub.editors[0].current!;
    model.setValue("[[");
    await flush();
    const { suggestions } = provider.provideCompletionItems(model as never, {
      lineNumber: 1,
      column: 3,
    });
    expect(suggestions.map((s) => s.insertText).sort()).toEqual(["[[测试稿]]", "[[甲笔记]]"]);
    // range 覆盖 [[ 前缀：startColumn 指向第一个 "["
    expect(suggestions.every((s) => (s.range as { startColumn: number }).startColumn === 1)).toBe(true);
  });

  it("wikiCompletion_insertsBracketPairVerbatim：筛词命中单候选，insertText 逐字 [[标题]] 且 range 括至光标", async () => {
    const provider = await openAndCaptureProvider();
    const model = stub.editors[0].current!;
    model.setValue("前文[[测");
    await flush();
    const { suggestions } = provider.provideCompletionItems(model as never, {
      lineNumber: 1,
      column: 6, // "前文[[测" 五字之后
    });
    expect(suggestions).toHaveLength(1);
    expect(suggestions[0].insertText).toBe("[[测试稿]]");
    expect(suggestions[0].label).toBe("测试稿（c.md）");
    expect(suggestions[0].range).toEqual({
      startLineNumber: 1,
      startColumn: 3, // 第一个「[」的 1 基列——range 覆盖 [[ 前缀
      endLineNumber: 1,
      endColumn: 6,
    });
    // 已闭合 [[x]] 不再弹候选
    model.setValue("已链[[甲笔记]]尾");
    await flush();
    expect(
      provider.provideCompletionItems(model as never, { lineNumber: 1, column: 11 }).suggestions,
    ).toEqual([]);
  });
});
