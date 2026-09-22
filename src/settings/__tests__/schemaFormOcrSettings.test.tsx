import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";

import SchemaForm, { enumChoices } from "../SchemaForm";
import { hostConfigGet, hostConfigSchema } from "../../ipc/client";
import OcrPanel, { langPlaceholder } from "../../modules/ocr/OcrPanel";
import {
  hostConfigSet,
  ocrConfigGet,
  ocrEngineStatus,
  ocrRecognize,
} from "../../ipc/client";

// D-29 B4/T-B4-10 回归：设置面说真话——有词表的 string 键渲染为下拉（杜绝手打引擎 id），
// 无词表的 string 键仍是自由文本 Input（对立面正对照，防空洞断言）；
// OCR 面板的语言多选只是「本次覆盖」，持久值只读显示，前端不把它塞回请求（单一真源）。

vi.mock("../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../ipc/client")>();
  return {
    ...actual,
    hostConfigSchema: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    ocrConfigGet: vi.fn(),
    ocrEngineStatus: vi.fn(),
    ocrRecognize: vi.fn(),
  };
});
vi.mock("../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(ocrConfigGet).mockResolvedValue({ langs: ["zh-CN"], preferred_engine: "win-ocr" });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
  vi.mocked(ocrEngineStatus).mockResolvedValue({
    engines: [{ id: "win-ocr", name: "Windows OCR", available: true }],
    languages: ["zh-CN", "en-US"],
  });
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

async function mount(jsx: ReactElement) {
  await act(async () => {
    root = createRoot(container);
    root.render(jsx);
  });
  await act(async () => {});
}

async function mountSettings(scheme: Record<string, unknown>) {
  vi.mocked(hostConfigSchema).mockResolvedValue({ properties: scheme } as never);
  vi.mocked(hostConfigGet).mockResolvedValue({} as never);
  await mount(<SchemaForm moduleId="ocr" />);
}

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(text));
}

/** 走真实隐藏 file input（同 ocrPanel.test.tsx 的通路）：文件→base64→request 对象 */
async function pickFile(name = "cap.png") {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]');
  expect(input).not.toBeNull();
  const file = new File(["x"], name, { type: "image/png" });
  await act(async () => {
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input!.dispatchEvent(new Event("change", { bubbles: true }));
  });
  for (let i = 0; i < 3; i += 1) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
  }
}

/**
 * Fluent v9 下拉：单选 Listbox 只在展开后入 DOM，多选选项常驻——统一先按真实指针序列
 * 展开触发钮，再按文本点选项（document 作用域取，兼容展开层落在 container 外）。
 */
async function clickOptionByText(text: string) {
  const trigger = document.querySelector<HTMLElement>('[role="combobox"]');
  expect(trigger, "缺下拉触发钮").toBeDefined();
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      trigger!.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
  const options = [...document.querySelectorAll<HTMLElement>(".fui-Option")];
  const target = options.find((o) => o.textContent?.includes(text));
  expect(target, `可选项：${options.map((o) => o.textContent).join("|")}`).toBeDefined();
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      target!.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
}

describe("SchemaForm enum 分支（T-B4-10）", () => {
  it("schemaForm_enumRendersDropdown_notFreeText：有词表的 string 键=下拉且无自由文本输入框", async () => {
    await mountSettings({
      preferred_engine: {
        type: "string",
        title: "优先 OCR 引擎",
        enum: ["win-ocr", "tesseract"],
        default: "win-ocr",
      },
    });
    const trigger = buttonByText("win-ocr");
    expect(trigger).toBeDefined();
    expect(trigger?.getAttribute("role")).toBe("combobox");
    expect(container.querySelector("input")).toBeNull();
    expect(container.textContent).toContain("优先 OCR 引擎");
    // 下拉不只是装饰：选中词表项即按该值写回（写侧仍只有 host_config_set 一口）
    await clickOptionByText("tesseract");
    expect(hostConfigSet).toHaveBeenCalledWith(
      "ocr",
      expect.objectContaining({ preferred_engine: "tesseract" }),
    );
  });

  it("schemaForm_stringStillInput_negativeControl：无 enum 的 string 键仍是 Input（零 churn 对立面）", async () => {
    await mountSettings({ hotkey: { type: "string", title: "快捷键" } });
    expect(container.querySelector("input")).not.toBeNull();
    expect(container.querySelector('[role="combobox"]')).toBeNull();
  });

  it("enumChoices 纯函数：空词表与畸形词表退回 Input，不渲染永远选不出项的下拉", () => {
    expect(enumChoices({ type: "string", enum: ["a", "b"] })).toEqual(["a", "b"]);
    expect(enumChoices({ type: "string" })).toBeNull();
    expect(enumChoices({ type: "string", enum: [] })).toBeNull();
    expect(enumChoices({ type: "string", enum: [""] })).toBeNull();
    expect(enumChoices({ type: "boolean", enum: ["a"] })).toBeNull();
  });
});

describe("OcrPanel 语言持久化显示（T-B4-10）", () => {
  it("ocrPanel_langsPlaceholder_showsPersistedDefault：占位文案读 ocr_config_get 的持久语言", async () => {
    await mount(<OcrPanel />);
    expect(ocrConfigGet).toHaveBeenCalled();
    const trigger = container.querySelector('[role="combobox"]');
    expect(trigger?.textContent).toContain("跟随设置（zh-CN）");
  });

  it("ocrPanel_selectionOverridesPersisted_forThisRun：手选 en-US ⇒ 本次请求交 [\"en-US\"]，配置零写入", async () => {
    vi.mocked(ocrRecognize).mockResolvedValue({
      lines: [],
      text: "",
      lang: "en-US",
      engine: "win-ocr",
    });
    await mount(<OcrPanel />);
    await clickOptionByText("en-US");
    await pickFile();
    expect(ocrRecognize).toHaveBeenCalledWith(expect.objectContaining({ langs: ["en-US"] }));
    // 红线：面板的多选只活在本次请求里——既不写设置，也不把配置值回填进请求
    expect(hostConfigSet).not.toHaveBeenCalled();
  });

  it("ocrPanel 未手选时请求仍交空数组（配置由后端解析，前端不越位塞值）", async () => {
    vi.mocked(ocrRecognize).mockResolvedValue({ lines: [], text: "", lang: "", engine: "win-ocr" });
    await mount(<OcrPanel />);
    await pickFile();
    expect(ocrRecognize).toHaveBeenCalledWith(expect.objectContaining({ langs: [] }));
  });

  it("langPlaceholder 纯函数三态：加载中 / 有持久值 / 未设", () => {
    expect(langPlaceholder(null)).toBe("语言：读取设置中…");
    expect(langPlaceholder(["zh-CN", "en-US"])).toBe("语言：跟随设置（zh-CN · en-US）");
    expect(langPlaceholder([])).toContain("引擎按系统语言自选");
  });
});
