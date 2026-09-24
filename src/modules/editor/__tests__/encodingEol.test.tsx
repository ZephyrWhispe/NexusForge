import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import EditorPanel from "../EditorPanel";
import {
  editorContent,
  editorOpen,
  editorRecoverDraft,
  editorSetEncoding,
  editorSessions,
  type EditorSessionInfoDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// T-B7-18 编码/EOL 真实切换 + autosave 回读（前端半）：
// ① open 检出较新草稿 → 提示恢复（确认走后端口、拒绝草稿留盘下次再提示）；
// ② 有损切换必须复述「将丢失 N 个字符」，用户拒绝即退回原档位；
// ③ EOL 下拉与编码下拉同走 editor_set_encoding 一条命令（eol 臂）。

vi.mock("../../../monaco/setup", () => {
  const fakeModel = () => ({
    isDisposed: () => false,
    setValue: () => {},
    getValue: () => "",
    getLanguageId: () => "plaintext",
    onDidChangeContent: () => ({ dispose() {} }),
    dispose: () => {},
  });
  return {
    languageForPath: () => "plaintext",
    monaco: {
      editor: {
        create: () => ({
          onDidChangeModelContent: () => ({ dispose() {} }),
          onDidScrollChange: () => ({ dispose() {} }),
          getModel: () => fakeModel(),
          setModel: () => {},
          updateOptions: () => {},
          getScrollTop: () => 0,
          getScrollHeight: () => 1,
          getLayoutInfo: () => ({ height: 100 }),
          setScrollTop: () => {},
          dispose: () => {},
        }),
        createModel: () => fakeModel(),
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
    editorContent: vi.fn(async () => "磁盘内容"),
    editorSessions: vi.fn(),
    editorSave: vi.fn(async () => undefined),
    editorSetEncoding: vi.fn(),
    editorRecoverDraft: vi.fn(),
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
  cursor_line: 1,
  opened_ms: 1000,
};

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

/** Fluent v9 下拉只在真实指针序列下展开（同 SchemaForm 测试惯例） */
async function pointerClick(el: HTMLElement) {
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      el.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
}

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
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

function comboboxByAria(label: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[role="combobox"][aria-label="${label}"]`);
  expect(el, `缺 aria-label=${label} 的下拉触发钮`).toBeDefined();
  return el!;
}

async function selectDropdownOption(label: string, optionText: string) {
  await pointerClick(comboboxByAria(label));
  const options = [...document.querySelectorAll<HTMLElement>(".fui-Option")];
  const target = options.find((o) => o.textContent?.trim() === optionText);
  expect(target, `可选项：${options.map((o) => o.textContent).join("|")}`).toBeDefined();
  await pointerClick(target!);
}

async function openViaUi() {
  await setInput(
    container.querySelector('input[placeholder^="文件绝对路径"]') as HTMLInputElement,
    "C:\\notes\\a.txt",
  );
  await click(buttonByText("打开")!);
}

async function renderPanel() {
  await act(async () => {
    root = createRoot(container);
    root.render(<EditorPanel />);
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  vi.mocked(confirmAction).mockResolvedValue(true);
  vi.mocked(editorSessions).mockResolvedValue([SESS_A]);
  vi.mocked(editorOpen).mockResolvedValue(SESS_A);
  vi.mocked(editorContent).mockResolvedValue("磁盘内容");
  container = document.createElement("div");
  document.body.append(container);
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
});

describe("EditorPanel 编码/EOL 切换与草稿回读（T-B7-18）", () => {
  it("autosaveDraft_resumesOnOpen", async () => {
    vi.mocked(editorOpen).mockResolvedValue({ ...SESS_A, autosave_draft: true });
    await renderPanel();
    await openViaUi();
    // 提示在场且确认词点名恢复
    const ask = vi.mocked(confirmAction).mock.calls.find(
      (c) => c[0].title === "发现未保存草稿",
    );
    expect(ask, "较新草稿必须提示恢复").toBeDefined();
    expect(vi.mocked(editorRecoverDraft).mock.calls).toEqual([["s1"]]);
  });

  it("autosaveDraft_declinedKeepsDraftForNextOpen", async () => {
    vi.mocked(editorOpen).mockResolvedValue({ ...SESS_A, autosave_draft: true });
    vi.mocked(confirmAction).mockResolvedValue(false);
    await renderPanel();
    await openViaUi();
    expect(editorRecoverDraft).not.toHaveBeenCalled(); // 拒绝=不动后端，草稿留盘
    expect(container.textContent).toContain("草稿保留");
  });

  it("editorPanel_encodingDropdown_lossyConfirmRecitesLossCount", async () => {
    vi.mocked(editorSetEncoding).mockResolvedValue({
      from: "utf8",
      to: "gbk",
      chars_before: 8,
      chars_after: 8,
      replacement_char_count: 2,
    });
    await renderPanel();
    await openViaUi();
    await selectDropdownOption("切换保存编码", "GBK");
    expect(editorSetEncoding).toHaveBeenCalledWith("s1", "gbk", "preserve");
    const ask = vi.mocked(confirmAction).mock.calls.find(
      (c) => c[0].title === "有损编码切换确认",
    );
    expect(ask, "有损切换必须弹确认").toBeDefined();
    const imp = ask![0].impact;
    expect(Array.isArray(imp) ? imp.join("\n") : String(imp)).toContain("将丢失 2 个不可映射字符"); // 复述判据
  });

  it("editorPanel_encodingDropdown_declineRevertsToPrev", async () => {
    vi.mocked(editorSetEncoding).mockResolvedValue({
      from: "utf8",
      to: "gbk",
      chars_before: 8,
      chars_after: 8,
      replacement_char_count: 2,
    });
    vi.mocked(confirmAction).mockImplementation(async (opts) => opts.title !== "有损编码切换确认");
    await renderPanel();
    await openViaUi();
    await selectDropdownOption("切换保存编码", "GBK");
    // 退回=第二次同命令，档位回到原编码（静默丢字与谎称成功同罪的反面）
    expect(vi.mocked(editorSetEncoding).mock.calls).toEqual([
      ["s1", "gbk", "preserve"],
      ["s1", "utf8", "preserve"],
    ]);
    expect(container.textContent).toContain("已退回原编码");
  });

  it("editorPanel_eolDropdown_sendsChoiceThroughSameCommand", async () => {
    await renderPanel();
    await openViaUi();
    await selectDropdownOption("切换行尾", "CRLF");
    expect(editorSetEncoding).toHaveBeenCalledWith("s1", "utf8", "crlf");
    expect(container.textContent).toContain("统一为 CRLF");
  });
});
