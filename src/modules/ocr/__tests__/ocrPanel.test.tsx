import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import OcrPanel, {
  confidence_reported,
  engineBadge,
  fmtConfidence,
  stripDataPrefix,
} from "../OcrPanel";
import {
  ocrConfigGet,
  ocrCopyText,
  ocrEngineStatus,
  ocrRecognize,
  type EngineStatusDto,
  type OcrResultDto,
} from "../../../ipc/client";

// D-29 B0/T-B0-3 回归：引擎卡字段逐一来自 ocr_engine_status；
// 引擎缺失时识别失败要"说明原因 + 给出首动作"，不得伪装成普通空态（00§4-4 / D-18）。
// T-B4-13 追加：置信度诚实位（不报就显"未提供"）+ 逐块复制 + 译文区只在真有值时出现。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    ocrEngineStatus: vi.fn(),
    ocrRecognize: vi.fn(),
    ocrCopyText: vi.fn(),
    ocrConfigGet: vi.fn(),
  };
});

const STATUS: EngineStatusDto = {
  engines: [
    { id: "win-ocr", name: "Windows OCR", available: true },
    { id: "tesseract", name: "Tesseract（未安装）", available: false },
  ],
  languages: ["zh-Hans", "en"],
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(ocrEngineStatus).mockResolvedValue(STATUS);
  // 设置里确有持久语言：若前端把配置塞进请求（第二真源），下面 langs: [] 的断言即红
  vi.mocked(ocrConfigGet).mockResolvedValue({
    langs: ["zh-Hans"],
    preferred_engine: "win-ocr",
    tesseract_enabled: false,
    tesseract_exe: "",
    tesseract_data_dir: null,
    tesseract_timeout_ms: 20000,
    translate_target_lang: "",
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

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<OcrPanel />);
  });
  await act(async () => {});
}

/** 走真实隐藏 file input + FileReader 通路，证明文件→base64→request 对象全链 */
async function pickFile(name = "cap.png") {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]');
  expect(input).not.toBeNull();
  const file = new File(["x"], name, { type: "image/png" });
  await act(async () => {
    Object.defineProperty(input, "files", { value: [file], configurable: true });
    input!.dispatchEvent(new Event("change", { bubbles: true }));
  });
  // FileReader 是宏任务：多冲刷几轮直到 recognize 的 promise 落定
  for (let i = 0; i < 3; i += 1) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
  }
}

function appError(code: string, message: string) {
  return { kind: "Module", data: { code, message } };
}

describe("OcrPanel（T-B0-3）", () => {
  it("ocrPanel_engineCard_fieldsFromStatus：引擎名/id/可用性/语言逐项来自状态命令", async () => {
    await mount();
    expect(ocrEngineStatus).toHaveBeenCalled();
    expect(container.textContent).toContain("Windows OCR");
    expect(container.textContent).toContain("win-ocr");
    expect(container.textContent).toContain("Tesseract（未安装）");
    expect(container.textContent).toContain("可用");
    expect(container.textContent).toContain("不可用");
    expect(container.textContent).toContain("支持语言：zh-Hans · en");
    expect(container.textContent).toContain("1/2 引擎可用");
  });

  it("ocrPanel_recognizeError_showsWhyEmpty：引擎缺失失败给出原因+首动作，且真调了 request 对象", async () => {
    vi.mocked(ocrRecognize).mockRejectedValue(
      appError("OCR_ENGINE_001", "所有 OCR 引擎均不可用: win-ocr 无 zh-Hans"),
    );
    await mount();
    await pickFile();
    expect(ocrRecognize).toHaveBeenCalledWith(
      expect.objectContaining({ image_b64: "eA==", langs: [] }),
    );
    expect(container.textContent).toContain("识别失败");
    expect(container.textContent).toContain("所有 OCR 引擎均不可用");
    // 首动作指引（不是光秃"暂无数据"）
    expect(container.textContent).toContain("刷新状态");
    expect(container.textContent).toContain("语音");
    expect(container.textContent).not.toContain("尚未识别");
  });

  it("成功路径：分行结果+置信度+复制全要走 ocr_copy_text", async () => {
    vi.mocked(ocrRecognize).mockResolvedValue({
      lines: [
        { text: "登录验证码 483920", rect: { x: 0, y: 0, w: 10, h: 4 }, confidence: 0.92 },
        { text: "请勿转发", rect: { x: 0, y: 5, w: 8, h: 3 }, confidence: 0.789 },
      ],
      text: "登录验证码 483920\n请勿转发",
      lang: "zh-Hans",
      engine: "win-ocr",
      engines_report_confidence: true,
    });
    vi.mocked(ocrCopyText).mockResolvedValue(undefined);
    await mount();
    await pickFile();
    expect(container.textContent).toContain("cap.png · 引擎 win-ocr · 语言 zh-Hans · 2 行");
    expect(container.textContent).toContain("登录验证码 483920");
    expect(container.textContent).toContain("92.0%");
    expect(container.textContent).toContain("78.9%");
    const copyBtn = [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("复制全部"),
    );
    expect(copyBtn).toBeDefined();
    await act(async () => {
      copyBtn!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(ocrCopyText).toHaveBeenCalledWith("登录验证码 483920\n请勿转发");
  });

  it("纯函数判据：stripDataPrefix / fmtConfidence / engineBadge（可失败自检）", () => {
    expect(stripDataPrefix("data:image/png;base64,QUJD")).toBe("QUJD");
    expect(stripDataPrefix("QUJD")).toBe("QUJD");
    expect(fmtConfidence(0.5)).toBe("50.0%");
    expect(fmtConfidence(1.2)).toBe("100.0%"); // 上钳
    expect(fmtConfidence(-0.3)).toBe("0.0%"); // 下钳
    expect(fmtConfidence(0.789)).toBe("78.9%");
    expect(engineBadge(null)).toBe("引擎状态加载中…");
    expect(engineBadge(STATUS)).toBe("1/2 引擎可用");
    expect(engineBadge({ engines: [], languages: [] })).toBe("0/0 引擎可用");
    // T-B4-13 诚实位：同一个 1.0，true 显百分数、false 显"未提供"（区分全在这一位）
    expect(confidence_reported(true, 1)).toBe("100.0%");
    expect(confidence_reported(false, 1)).toBe("未提供");
    expect(confidence_reported(false, 0.42)).toBe("未提供");
  });
});

// ---------------- T-B4-13（09 §9.2）：结果诚实化 + 按块复制 + 翻译槽表观 ----------------

const THREE_LINES: OcrResultDto = {
  lines: [
    { text: "第一行", rect: { x: 0, y: 0, w: 6, h: 2 }, confidence: 1.0 },
    { text: "第二行", rect: { x: 0, y: 3, w: 6, h: 2 }, confidence: 1.0 },
    { text: "第三行", rect: { x: 0, y: 6, w: 6, h: 2 }, confidence: 1.0 },
  ],
  text: "第一行\n第二行\n第三行",
  lang: "zh-Hans",
  engine: "win-ocr",
  engines_report_confidence: true,
};

/** 识别一次的完整通路（成功路径夹具共用） */
async function recognizeWith(result: OcrResultDto) {
  vi.mocked(ocrRecognize).mockResolvedValue(result);
  vi.mocked(ocrCopyText).mockResolvedValue(undefined);
  await mount();
  await pickFile();
}

/** 同一用例内换结果重挂（root/container 是模块级单例，二次 mount 先卸干净） */
async function remountWith(result: OcrResultDto) {
  await act(async () => {
    try {
      root?.unmount();
    } catch {
      /* 已卸载 */
    }
  });
  container.remove();
  container = document.createElement("div");
  document.body.append(container);
  await recognizeWith(result);
}

function buttonsWith(label: string): HTMLButtonElement[] {
  return [...container.querySelectorAll("button")].filter((b) =>
    b.textContent?.includes(label),
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

describe("结果诚实化与翻译槽（T-B4-13）", () => {
  it("ocrPanel_perBlockCopy_invokesWithThatLineOnly", async () => {
    await recognizeWith(THREE_LINES);
    const btns = buttonsWith("复制此块");
    // 每行一枚：按块复制是行级操作
    expect(btns).toHaveLength(3);
    await click(btns[1]);
    // 字面判据：只交该行文本，不误带邻行
    expect(ocrCopyText).toHaveBeenCalledTimes(1);
    expect(ocrCopyText).toHaveBeenCalledWith("第二行");
  });

  it("ocrPanel_confidenceUnreported_showsNotProvided", async () => {
    // 红线：win-ocr 的 1.0 是"未知"的占位，不得渲染成 100.0%
    await recognizeWith({ ...THREE_LINES, engines_report_confidence: false });
    expect(container.textContent).toContain("未提供");
    expect(container.textContent).not.toContain("100.0%");
    // 正对照：同一批数值，true 时按百分数显示（区分只在那一位）
    await remountWith({ ...THREE_LINES, engines_report_confidence: true });
    expect(container.textContent).toContain("100.0%");
    expect(container.textContent).not.toContain("未提供");
  });

  it("ocrPanel_translateBadgePresent", async () => {
    await recognizeWith(THREE_LINES);
    expect(container.textContent).toContain("翻译 · 延后");
    const badge = [...container.querySelectorAll("[title]")].find((e) =>
      e.getAttribute("title")?.includes("翻译"),
    );
    expect(badge?.getAttribute("title")).toContain("D-08");
  });

  it("ocrPanel_translateResultSectionAppearsOnlyWhenNonNull", async () => {
    // 负例：后端 skip null（两键都不在）时零空节，不留一个空标题骗眼睛
    await recognizeWith(THREE_LINES);
    expect(container.textContent).not.toContain("译文");
    // 正对照：真有译文才出节，内容逐字来自后端
    await remountWith({ ...THREE_LINES, translate: "[en]line one" });
    expect(container.textContent).toContain("译文");
    expect(container.textContent).toContain("[en]line one");
  });

  it("辅测 ocrPanel_translateError_shownVerbatim：失败原因摊开且不顶掉译文位", async () => {
    await recognizeWith({ ...THREE_LINES, translate_error: "v1 未接入翻译服务（D-08）" });
    expect(container.textContent).toContain("译文未完成：v1 未接入翻译服务（D-08）");
    // 识别主体照常在场（附加功能失败 ≠ 整次失败）
    expect(container.textContent).toContain("第一行");
    expect(buttonsWith("复制全部")).toHaveLength(1);
  });
});
