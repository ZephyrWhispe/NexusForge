import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import EditorPanel from "../EditorPanel";
import {
  editorContent,
  editorOpen,
  editorSaveAs,
  editorSessions,
  type EditorSessionInfoDto,
} from "../../../ipc/client";

// D-29 B1/T-B1-10 回归：另存为——目标路径 Input 沿用打开惯例（trim+Enter），
// 另存为按钮在保存按钮右侧；点击后 editorSaveAs(id, target) 真 invoke，
// 会话经后端 save_as 换绑 path，页签名按 refreshSessions（editorSessions 真相源）刷新。
// monaco 全量 stub（首个 EditorPanel 测试立此先例，同 TerminalPanel 的 xterm stub 思路）。

vi.mock("../../../monaco/setup", () => {
  class FakeModel {
    private value = "";
    isDisposed() {
      return false;
    }
    setValue(v: string) {
      this.value = v;
    }
    getValue() {
      return this.value;
    }
    getLanguageId() {
      return "plaintext";
    }
    onDidChangeContent() {
      return { dispose() {} };
    }
    dispose() {}
  }
  return {
    languageForPath: () => "plaintext",
    monaco: {
      editor: {
        create: () => ({
          onDidChangeModelContent: () => ({ dispose() {} }),
          onDidScrollChange: () => ({ dispose() {} }),
          getModel: () => null,
          setModel: () => {},
          updateOptions: () => {},
          getScrollTop: () => 0,
          getScrollHeight: () => 1,
          getLayoutInfo: () => ({ height: 100 }),
          setScrollTop: () => {},
          dispose: () => {},
        }),
        createModel: () => new FakeModel(),
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
    editorSaveAs: vi.fn(),
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
  eol: "lf",
  eol_mixed: false,
  dirty: false,
  size: 8,
  big_file: false,
  readonly: false,
};
const SESS_B: EditorSessionInfoDto = {
  ...SESS_A,
  path: "D:\\backup\\renamed.txt",
  name: "renamed.txt",
};

let container: HTMLDivElement;
let root: Root;
// 会话列表真相源开关：另存为成功后翻到 [SESS_B]（模拟后端换绑）
let liveList: EditorSessionInfoDto[] = [SESS_A];

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

function inputByPlaceholder(prefix: string): HTMLInputElement {
  const el = container.querySelector(`input[placeholder^="${prefix}"]`);
  expect(el, `placeholder 前缀「${prefix}」的输入框应存在`).not.toBeNull();
  return el as HTMLInputElement;
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

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  liveList = [SESS_A];
  vi.mocked(editorSessions).mockImplementation(async () => liveList);
  vi.mocked(editorOpen).mockResolvedValue(SESS_A);
  vi.mocked(editorContent).mockResolvedValue("内容");
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

describe("EditorPanel 另存为（T-B1-10）", () => {
  it("editorSaveAs_pathInvokesAndRetitlesTab：无会话无入口→填路径→invoke→页签换名", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<EditorPanel />);
    });
    await act(async () => {});
    // 负例臂：无活跃会话时既无另存为入口也无目标输入框（不空转 invoke）
    expect(buttonByText("另存为")).toBeUndefined();
    expect(container.querySelector('input[placeholder^="另存为"]')).toBeNull();

    // 打开会话（沿用面板真实路径：doOpen → refreshSessions → setActiveId）
    await setInput(inputByPlaceholder("文件绝对路径"), "C:\\notes\\a.txt");
    await click(buttonByText("打开")!);
    expect(editorOpen).toHaveBeenCalledWith("C:\\notes\\a.txt");
    expect(container.textContent).toContain("a.txt");

    // busy/空路径门禁：未填目标 → 禁用（jsdom 对 disabled 按钮仍派发 click，门禁钉属性）
    const saveAsBtn = buttonByText("另存为")!;
    expect(saveAsBtn.disabled).toBe(true);
    expect(saveAsBtn.title).toContain("换绑");

    // 另存为：id 原样 + trim 后目标路径逐字节进 invoke
    vi.mocked(editorSaveAs).mockImplementation(async () => {
      liveList = [SESS_B]; // 后端换绑生效后真相源
      return SESS_B;
    });
    await setInput(inputByPlaceholder("另存为"), "  D:\\backup\\renamed.txt  ");
    await click(saveAsBtn);
    expect(editorSaveAs).toHaveBeenCalledTimes(1);
    expect(editorSaveAs).toHaveBeenCalledWith("s1", "D:\\backup\\renamed.txt");
    // 页签按返回后的会话列表换名；状态行点名新文件名与编码/EOL
    expect(container.textContent).toContain("renamed.txt");
    expect(container.textContent).not.toContain("a.txt");
    expect(container.textContent).toContain("已另存为 renamed.txt（UTF-8 / LF）");
    // 成功后输入框清空，不留陈旧目标
    expect(inputByPlaceholder("另存为").value).toBe("");
  });
});
