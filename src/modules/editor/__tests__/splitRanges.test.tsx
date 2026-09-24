import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import EditorPanel, { parse_page_ranges } from "../EditorPanel";
import { pdfSplit } from "../../../ipc/client";

// T-B7-19 PDF 拆分页码区间（前端半）：输入框 "2-4,7" 经同名纯函数 parse_page_ranges
// 解析后以数组过 IPC（Rust 侧只收已解析数组）；空输入=全拆（undefined→None）；
// 畸形区间在**发命令前**逐条点名拒——pdfSplit 一次都不许被调用。

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
    editorSessions: vi.fn(async () => []),
    pdfSplit: vi.fn(async () =>
      [2, 3, 4, 7].map((p) => ({
        output: `C:\\x\\doc_${p}.pdf`,
        pages: 1,
        size: 1024,
      })),
    ),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

let container: HTMLDivElement;
let root: Root;

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

async function renderPanel() {
  await act(async () => {
    root = createRoot(container);
    root.render(<EditorPanel />);
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
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

describe("EditorPanel PDF 拆分页码区间（T-B7-19）", () => {
  it("editorPanel_splitDialog_rangeInputRoundTrip", async () => {
    // 纯函数臂：与 Rust parse_page_ranges 同语义（升序去重 + 逐条点名）
    expect(parse_page_ranges("2-4,7")).toEqual([2, 3, 4, 7]);
    expect(parse_page_ranges("7,2-4,3")).toEqual([2, 3, 4, 7]);
    for (const bad of ["4-", "a", "0", "-2", "7-3"]) {
      expect(() => parse_page_ranges(bad), `'{bad}' 须点名拒`).toThrow(bad === "0" ? "0" : bad);
    }
    await renderPanel();
    const pathInput = container.querySelector(
      'input[placeholder^="PDF 绝对路径"]',
    ) as HTMLInputElement;
    const pagesInput = container.querySelector(
      'input[placeholder^="拆分页码区间"]',
    ) as HTMLInputElement;
    expect(pagesInput, "缺拆分页码输入框").not.toBeNull();

    // 区间臂：UI 文本 → IPC 已解析数组（后端不收字符串）
    await setInput(pathInput, "C:\\x\\doc.pdf");
    await setInput(pagesInput, "2-4,7");
    await click(buttonByText("拆分")!);
    expect(pdfSplit).toHaveBeenCalledWith("C:\\x\\doc.pdf", "C:\\x\\doc.pdf_pages", [
      2, 3, 4, 7,
    ]);
    expect(container.textContent).toContain("第 2,3,4,7 页");

    // 空=全拆臂：undefined 过 IPC（Rust None 语义）
    await setInput(pagesInput, "");
    await click(buttonByText("拆分")!);
    expect(pdfSplit).toHaveBeenLastCalledWith("C:\\x\\doc.pdf", "C:\\x\\doc.pdf_pages", undefined);

    // 畸形臂：发命令前点名拒，pdfSplit 不得再被调用
    const callsBefore = vi.mocked(pdfSplit).mock.calls.length;
    await setInput(pagesInput, "7-3");
    await click(buttonByText("拆分")!);
    expect(vi.mocked(pdfSplit).mock.calls.length).toBe(callsBefore);
    expect(container.textContent).toContain("倒序");
  });
});
