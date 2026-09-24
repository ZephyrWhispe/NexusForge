import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import EditorPanel from "../EditorPanel";
import {
  editorAutosave,
  editorContent,
  editorOpen,
  editorSessions,
  type EditorSessionInfoDto,
} from "../../../ipc/client";

// D-29 B1 批次尾实启冒烟缺陷回归（两条）：
// ① 编辑器创建 effect 的 deps 曾是 [activeId, sessions]——每次会话刷新/脏标记都
//    dispose 重建编辑器与全部 model，在途 autosave 定时器落在已 dispose 的 model 上
//    抛 "Model is disposed!"（宿主日志实锤）→ 现 deps 为「是否存在活跃会话」布尔，
//    全程只建一次编辑器（本文件第 5 例同时钉死这条：初版误写 [] 时它当场红）；
// ② 打开时程序化 model.setValue 曾同步触发变更监听 → 未编辑即置脏 + 3s 后写草稿
//    → loadingLoadRef 抑制；定时器还需在卸载时清理、model dispose 后跳过。

const stub = vi.hoisted(() => ({
  createCount: 0,
  modelsDisposed: [] as string[],
  editors: [] as Array<{
    handlers: Array<() => void>;
    current: { setValue: (v: string) => void; getValue: () => string; dispose: () => void } | null;
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
      return "plaintext";
    }
    onDidChangeContent() {
      return { dispose() {} };
    }
    dispose() {
      this.disposed = true;
    }
  }
  return {
    languageForPath: () => "plaintext",
    monaco: {
      editor: {
        create: () => {
          stub.createCount += 1;
          const editor = {
            handlers: [] as Array<() => void>,
            current: null as FakeModel | null,
          };
          stub.editors.push(editor as unknown as (typeof stub.editors)[number]);
          return {
            onDidChangeModelContent: (h: () => void) => {
              editor.handlers.push(h);
              return { dispose() {} };
            },
            onDidScrollChange: () => ({ dispose() {} }),
            getModel: () => editor.current,
            setModel: (m: FakeModel) => {
              editor.current = m;
            },
            updateOptions: () => {},
            getScrollTop: () => 0,
            getScrollHeight: () => 1,
            getLayoutInfo: () => ({ height: 100 }),
            setScrollTop: () => {},
            dispose: () => {},
          };
        },
        createModel: () => {
          const editor = stub.editors[stub.editors.length - 1];
          return new FakeModel(() => editor.handlers.forEach((h) => h()));
        },
        setModelLanguage: () => {},
      },
    },
  };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    editorOpen: vi.fn(),
    editorContent: vi.fn(),
    editorSessions: vi.fn(),
    editorAutosave: vi.fn(async () => false),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const SESS_A: EditorSessionInfoDto = {
  id: "s1",
  path: "C:\\notes\\a.txt",
  name: "a.txt",
  encoding: "utf8",
  encoding_label: "UTF-8",
  preferred_encoding: null,
  autosave_draft: false,
  eol: "lf",
  eol_mixed: false,
  dirty: false,
  size: 8,
  big_file: false,
  readonly: false,
};
const SESS_B: EditorSessionInfoDto = {
  ...SESS_A,
  id: "s2",
  path: "C:\\notes\\b.txt",
  name: "b.txt",
};

let container: HTMLDivElement;
let root: Root;
let liveList: EditorSessionInfoDto[] = [SESS_A];

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function advance(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
  });
}

function currentModel() {
  const ed = stub.editors[stub.editors.length - 1];
  expect(ed.current, "编辑器应已挂上当前 model").not.toBeNull();
  return ed.current!;
}

async function openSessionA() {
  await setInput(
    container.querySelector('input[placeholder^="文件绝对路径"]') as HTMLInputElement,
    "C:\\notes\\a.txt",
  );
  await click(buttonByText("打开")!);
  await act(async () => {}); // editorContent 微任务链落定
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers();
  vi.clearAllMocks();
  stub.createCount = 0;
  stub.editors.length = 0;
  liveList = [SESS_A];
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(editorSessions).mockImplementation(async () => liveList);
  vi.mocked(editorOpen).mockResolvedValue(SESS_A);
  vi.mocked(editorContent).mockResolvedValue("磁盘内容");
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
  vi.useRealTimers();
});

describe("EditorPanel 脏标记/autosave 纪律（B1 批次尾冒烟缺陷）", () => {
  it("editorProgrammaticLoadKeepsCleanAndQuiet：打开载入不置脏、3s 后零 autosave", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await openSessionA();
    expect(container.textContent).toContain("a.txt");
    expect(container.textContent).not.toContain("●"); // 未编辑不得现脏点
    await advance(3500);
    expect(editorAutosave).not.toHaveBeenCalled(); // 更不得写崩溃草稿
    expect(currentModel().getValue).toBeDefined();
  });

  it("editorUserEditMarksDirtyAndAutosavesDraft：真实编辑才置脏并 3s 防抖存草稿", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await openSessionA();
    await act(async () => {
      currentModel().setValue("用户输入");
    });
    expect(container.textContent).toContain("●");
    await advance(2000);
    expect(editorAutosave).not.toHaveBeenCalled(); // 防抖未满不抢跑
    await advance(1500);
    expect(editorAutosave).toHaveBeenCalledTimes(1);
    expect(editorAutosave).toHaveBeenCalledWith("s1", "用户输入");
  });

  it("editorUnmountClearsPendingAutosave：卸载后在途定时器不得复活", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await openSessionA();
    await act(async () => {
      currentModel().setValue("还没存就切走");
    });
    await advance(1000); // 定时器在途
    act(() => {
      root.unmount();
    });
    await advance(5000);
    expect(editorAutosave).not.toHaveBeenCalled();
  });

  it("editorAutosaveSkipsDisposedModel：model 先销毁则定时器静默跳过而非抛错", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await openSessionA();
    const model = currentModel();
    await act(async () => {
      model.setValue("编辑中");
    });
    await advance(1000);
    act(() => {
      model.dispose(); // 换绑/重建竞态：面板仍在，model 已销毁
    });
    await expect(advance(3000)).resolves.toBeUndefined();
    expect(editorAutosave).not.toHaveBeenCalled();
  });

  it("editorSessionsChangeDoesNotRecreateEditor：会话刷新/切会话全程只建一次编辑器", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await openSessionA(); // sessions（挂载拉取 + 打开后刷新）与 activeId 均变
    expect(stub.createCount).toBe(1);
    await act(async () => {
      currentModel().setValue("编辑"); // 脏标记 → sessions 再变
    });
    expect(stub.createCount).toBe(1);
    liveList = [{ ...SESS_A, dirty: true }, SESS_B]; // 后端真相源：s1 编辑中 dirty=true
    vi.mocked(editorOpen).mockResolvedValue(SESS_B); // 面板真实路径：doOpen→refreshSessions→setActiveId
    await setInput(
      container.querySelector('input[placeholder^="文件绝对路径"]') as HTMLInputElement,
      "C:\\notes\\b.txt",
    );
    await click(buttonByText("打开")!);
    await act(async () => {}); // s2 的 editorContent 载入链落定
    expect(stub.createCount).toBe(1);
    expect(container.textContent).toContain("b.txt");
    expect(container.textContent).toContain("●"); // a.txt 在编辑中置的脏点不因切会话丢失
  });
});
