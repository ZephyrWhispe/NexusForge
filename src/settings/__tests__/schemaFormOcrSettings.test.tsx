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
// D-41 D5：表单新增 nf:event 监听（host.config_rejected 显影），jsdom 无 Tauri 内部口
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
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
  vi.mocked(ocrConfigGet).mockResolvedValue({
    langs: ["zh-CN"],
    preferred_engine: "win-ocr",
    tesseract_enabled: false,
    tesseract_exe: "",
    tesseract_data_dir: null,
    tesseract_timeout_ms: 20000,
    translate_target_lang: "",
  });
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

/** Fluent v9 下拉只在真实指针序列下展开（单选 Listbox 展开后才入 DOM） */
async function pointerClick(el: HTMLElement) {
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      el.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
}

/** 展开页面上唯一的单选下拉（点一次触发钮），返回展开后的选项文本 */
async function openDropdown(): Promise<string[]> {
  const trigger = document.querySelector<HTMLElement>('[role="combobox"]');
  expect(trigger, "缺下拉触发钮").toBeDefined();
  await pointerClick(trigger!);
  return [...document.querySelectorAll<HTMLElement>(".fui-Option")].map(
    (o) => o.textContent ?? "",
  );
}

/** 展开后按文本点选项（document 作用域取，兼容展开层落在 container 外） */
async function clickOptionByText(text: string) {
  await openDropdown();
  const options = [...document.querySelectorAll<HTMLElement>(".fui-Option")];
  const target = options.find((o) => o.textContent?.includes(text));
  expect(target, `可选项：${options.map((o) => o.textContent).join("|")}`).toBeDefined();
  await pointerClick(target!);
}

/** React 受控 Input 写值：走原生 value setter，否则 onChange 收不到变化 */
async function typeInput(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => {});
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
      engines_report_confidence: true,
    });
    await mount(<OcrPanel />);
    await clickOptionByText("en-US");
    await pickFile();
    expect(ocrRecognize).toHaveBeenCalledWith(expect.objectContaining({ langs: ["en-US"] }));
    // 红线：面板的多选只活在本次请求里——既不写设置，也不把配置值回填进请求
    expect(hostConfigSet).not.toHaveBeenCalled();
  });

  it("ocrPanel 未手选时请求仍交空数组（配置由后端解析，前端不越位塞值）", async () => {
    vi.mocked(ocrRecognize).mockResolvedValue({
      lines: [],
      text: "",
      lang: "",
      engine: "win-ocr",
      engines_report_confidence: true,
    });
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

// D-29 B4/T-B4-11 回归：第二引擎（Tesseract CLI）表观面——启用后引擎状态卡与优先引擎
// 词表同步长出它；路径写错时后端 001 文案原样可见（不静默回滚成"看起来保存成功"）。

/** 换一棵树：卸载 + 换新容器（同一用例内多次 mount 时用，避免 React 双 root 警告） */
async function remount(jsx: ReactElement) {
  await act(async () => {
    root?.unmount();
  });
  container.remove();
  container = document.createElement("div");
  document.body.append(container);
  await mount(jsx);
}

/** ocr 模块的实时 schema 形状（七键；enum 由后端按引擎注册表给出） */
const OCR_SCHEMA = {
  preferred_engine: {
    type: "string",
    title: "优先 OCR 引擎",
    enum: ["win-ocr", "tesseract"],
    default: "win-ocr",
  },
  langs: { type: "array", items: { type: "string" }, title: "偏好语言", default: [] },
  tesseract_enabled: { type: "boolean", title: "启用 Tesseract 引擎（本地命令行）", default: false },
  tesseract_exe: { type: "string", title: "Tesseract 可执行文件（绝对路径）", default: "" },
  tesseract_data_dir: { type: "string", title: "tessdata 目录（留空 = 用引擎自带缺省）", default: "" },
  tesseract_timeout_ms: {
    type: "integer",
    title: "单次识别超时（毫秒）",
    minimum: 1000,
    maximum: 120000,
    default: 20000,
  },
  translate_target_lang: {
    type: "string",
    title: "译文目标语言（留空 = 不译）",
    default: "",
  },
};

const TWO_ENGINES = {
  engines: [
    { id: "win-ocr", name: "Windows.Media.Ocr", available: true },
    { id: "tesseract", name: "Tesseract（本地命令行）", available: false },
  ],
  languages: ["zh-CN", "en-US", "chi_sim"],
};
const ONE_ENGINE = {
  engines: [{ id: "win-ocr", name: "Windows.Media.Ocr", available: true }],
  languages: ["zh-CN", "en-US"],
};

describe("Tesseract 第二引擎表观面（T-B4-11）", () => {
  it("ocrPanel_secondEngineAppearsAfterEnable：两引擎→引擎卡两行 + 优先引擎下拉两项", async () => {
    vi.mocked(ocrEngineStatus).mockResolvedValue(TWO_ENGINES);
    await mount(<OcrPanel />);
    expect(container.textContent).toContain("1/2 引擎可用");
    expect(container.textContent).toContain("Tesseract（本地命令行）");
    expect(container.textContent).toContain("chi_sim");
    // 正对照：关着的引擎不进注册表 → 面板只有一张卡（"两行"不是恒定表观）
    vi.mocked(ocrEngineStatus).mockResolvedValue(ONE_ENGINE);
    await remount(<OcrPanel />);
    expect(container.textContent).toContain("1/1 引擎可用");
    expect(container.textContent).not.toContain("Tesseract");
    // 启用后设置中心的优先引擎词表跟着长（enum 来自注册表，非前端硬编码）
    vi.mocked(hostConfigSchema).mockResolvedValue({ properties: OCR_SCHEMA } as never);
    vi.mocked(hostConfigGet).mockResolvedValue({} as never);
    await remount(<SchemaForm moduleId="ocr" />);
    const trigger = document.querySelector<HTMLElement>('[role="combobox"]');
    expect(trigger?.textContent).toContain("win-ocr");
    // 展开一次取全部词表项（再点触发钮会收起，故选项直接在展开态里点）
    expect(await openDropdown()).toEqual(["win-ocr", "tesseract"]);
    const target = [...document.querySelectorAll<HTMLElement>(".fui-Option")].find((o) =>
      o.textContent?.includes("tesseract"),
    );
    expect(target, "词表应含第二引擎 tesseract").toBeDefined();
    await pointerClick(target!);
    expect(hostConfigSet).toHaveBeenCalledWith(
      "ocr",
      expect.objectContaining({ preferred_engine: "tesseract" }),
    );
  });

  it("ocrSettings_tesseractExe_requiresAbsoluteHint：后端 001 文案原样显示，不静默", async () => {
    vi.mocked(hostConfigSchema).mockResolvedValue({ properties: OCR_SCHEMA } as never);
    vi.mocked(hostConfigGet).mockResolvedValue({ tesseract_enabled: true } as never);
    await mount(<SchemaForm moduleId="ocr" />);
    const exeInput = container.querySelector<HTMLInputElement>('input[type="text"]');
    expect(exeInput, "无词表的 string 键渲染为 Input").not.toBeNull();
    expect(container.textContent).toContain("启用 Tesseract 引擎");
    expect(container.textContent).toContain("单次识别超时");
    vi.mocked(hostConfigSet).mockRejectedValue({
      data: {
        code: "OCR_TESS_001",
        message: "Tesseract 可执行文件必须是绝对路径，当前值：tesseract.exe",
        hint: "不吃相对路径与裸程序名：那会走 PATH 解析，等于给任意同名程序开门",
      },
    });
    await typeInput(exeInput!, "tesseract.exe");
    // PERF-02：文本输入 400ms 防抖合并提交——推进防抖窗后落盘
    await act(async () => {
      await new Promise((r) => setTimeout(r, 450));
    });
    expect(hostConfigSet).toHaveBeenCalledWith(
      "ocr",
      expect.objectContaining({ tesseract_exe: "tesseract.exe" }),
    );
    expect(container.textContent).toContain("必须是绝对路径，当前值：tesseract.exe");
    expect(container.textContent).toContain("PATH 解析");
    // 红线：拒写既不吞原因，也不吞用户输入（框里仍是他打的值，改了即可再试）
    expect(exeInput!.value).toBe("tesseract.exe");
    // 正对照：填对路径后错误行消失
    vi.mocked(hostConfigSet).mockResolvedValue(undefined);
    await typeInput(exeInput!, "C:\\Program Files\\Tesseract-OCR\\tesseract.exe");
    await act(async () => {
      await new Promise((r) => setTimeout(r, 450));
    });
    expect(container.textContent).not.toContain("必须是绝对路径");
  });
});
