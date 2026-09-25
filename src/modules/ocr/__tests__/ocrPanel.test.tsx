import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import OcrPanel, {
  confidence_reported,
  engineBadge,
  fmtConfidence,
  mergeOcrTexts,
  queueStatusBadge,
  stripDataPrefix,
  type OcrQueueItem,
} from "../OcrPanel";
import panelSrc from "../OcrPanel.tsx?raw";
import {
  ocrConfigGet,
  ocrCopyText,
  ocrEngineStatus,
  ocrExport,
  ocrRecognize,
  type EngineStatusDto,
  type OcrResultDto,
} from "../../../ipc/client";

// D-29 B0/T-B0-3 回归：引擎卡字段逐一来自 ocr_engine_status；
// 引擎缺失时识别失败要"说明原因 + 给出首动作"，不得伪装成普通空态（00§4-4 / D-18）。
// T-B4-13 追加：置信度诚实位（不报就显"未提供"）+ 逐块复制 + 译文区只在真有值时出现。
// T-B4-12 追加：多选成批 + 串行识别（一张在途）+ 失败行保留 + 合并导出 txt/md + 清空即停。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    ocrEngineStatus: vi.fn(),
    ocrRecognize: vi.fn(),
    ocrCopyText: vi.fn(),
    ocrConfigGet: vi.fn(),
    ocrExport: vi.fn(),
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

// ---------------- T-B4-12（09 §9.2）：批量识别 + 合并导出（§9.1-⑮ 收窄的机器面） ----------------

/** 放行闸：每张图的识别 promise 由测试手动了结，用来证明"同一时刻只有一张在途" */
function gated(res: OcrResultDto, failB64?: string) {
  const gates: { release: () => void }[] = [];
  vi.mocked(ocrRecognize).mockImplementation((req) => {
    let settle: (v: OcrResultDto) => void = () => {};
    let reject: (e: unknown) => void = () => {};
    const p = new Promise<OcrResultDto>((res2, rej2) => {
      settle = res2;
      reject = rej2;
    });
    gates.push({
      release: () => {
        if (failB64 && req.image_b64 === failB64) {
          reject(appError("OCR_INPUT_002", "这张图的 Base64 解码失败"));
        } else {
          settle({ ...res, text: `${res.text}#${req.image_b64}` });
        }
      },
    });
    return p;
  });
  return gates;
}

async function flush(rounds = 6) {
  for (let i = 0; i < rounds; i += 1) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 5));
    });
  }
}

/** 一次交多张图（内容各异 → base64 各异，于是"调用序"是可观测的） */
async function pickFiles(files: [name: string, content: string][]) {
  const input = container.querySelector<HTMLInputElement>('input[type="file"]');
  expect(input, "隐藏的 file input 应在册").not.toBeNull();
  expect(input!.multiple, "v1 批量走原生 multiple，而非原生窗口级拖放").toBe(true);
  await act(async () => {
    Object.defineProperty(input, "files", {
      value: files.map(([name, content]) => new File([content], name, { type: "image/png" })),
      configurable: true,
    });
    input!.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

const OK_RESULT: OcrResultDto = { ...THREE_LINES, text: "正文" };

describe("批量识别与合并导出（T-B4-12）", () => {
  it("ocrBatch_multipleFiles_recognizesSequentially_keepsFailedRows", async () => {
    // b.png 的内容 "B" → base64 "Qg=="：中间那张必败
    const gates = gated(OK_RESULT, "Qg==");
    await mount();
    await pickFiles([
      ["a.png", "A"],
      ["b.png", "B"],
      ["c.png", "C"],
    ]);
    await flush();
    // 串行证明（不是"三张一起发"）：三张在队，在途只有第一张
    expect(ocrRecognize).toHaveBeenCalledTimes(1);
    expect(gates).toHaveLength(1);
    for (let guard = 0; guard < 5; guard += 1) {
      const g = gates.shift();
      if (!g) break;
      g.release();
      await flush(3);
    }
    await flush();
    expect(ocrRecognize).toHaveBeenCalledTimes(3);
    // 调用序 = 文件名序（每张图的 base64 不同，故序可观测）
    expect(vi.mocked(ocrRecognize).mock.calls.map(([r]) => r.image_b64)).toEqual([
      "QQ==",
      "Qg==",
      "Qw==",
    ]);
    expect(container.textContent).toContain("a.png");
    expect(container.textContent).toContain("c.png");
    expect(container.textContent).toContain("已识别");
    // 红线：整批不中断 + 失败行保留原因（不是静默丢文件）
    expect(container.textContent).toContain("失败");
    expect(container.textContent).toContain("这张图的 Base64 解码失败");
    expect(container.textContent).toContain("3/3 已完成");
  });

  it("ocrBatch_exportInvokesMergedText", async () => {
    vi.mocked(ocrRecognize)
      .mockResolvedValueOnce({ ...OK_RESULT, text: "甲文" })
      .mockRejectedValueOnce(appError("OCR_INPUT_002", "坏图"))
      .mockResolvedValueOnce({ ...OK_RESULT, text: "丙文" });
    vi.mocked(ocrExport).mockResolvedValue("C:/appdata/export/ocr-1.txt");
    await mount();
    await pickFiles([
      ["a.png", "A"],
      ["b.png", "B"],
      ["c.png", "C"],
    ]);
    await flush();
    await click(buttonsWith("导出合并 txt")[0]);
    await flush(2);
    // 字面判据：失败行同样进节（写"识别失败：原因"），格式钮交 "txt"
    expect(ocrExport).toHaveBeenCalledWith(
      expect.stringContaining("===== b.png ====="),
      "txt",
    );
    const merged = vi.mocked(ocrExport).mock.calls[0][0];
    expect(merged).toContain("甲文");
    expect(merged).toContain("识别失败：坏图");
    expect(merged).toContain("丙文");
  });

  it("ocrBatch_mergeTxtAndMd_pinned", () => {
    const items: OcrQueueItem[] = [
      { name: "a.png", size: 10, status: "ok", chars: 2, result: { ...OK_RESULT, text: "甲" } },
      { name: "b.png", size: 10, status: "fail", chars: 0, reason: "引擎不可用" },
      { name: "c.png", size: 10, status: "pending", chars: 0 },
    ];
    expect(mergeOcrTexts(items, "txt")).toBe(
      "===== a.png =====\n甲\n\n===== b.png =====\n识别失败：引擎不可用",
    );
    expect(mergeOcrTexts(items, "md")).toBe(
      "## a.png\n\n```\n甲\n```\n\n## b.png\n\n```\n识别失败：引擎不可用\n```",
    );
    // pending 不进导出（钮在跑批时禁用）；已落定的两行一节不少
    expect(mergeOcrTexts(items, "txt")).not.toContain("c.png");
    expect(queueStatusBadge("pending")).toBe("排队中");
    expect(queueStatusBadge("ok")).toBe("已识别");
    expect(queueStatusBadge("fail")).toBe("失败");
  });

  it("ocrBatch_usesMultipleInput_notNativeDrag", async () => {
    await mount();
    const input = container.querySelector<HTMLInputElement>('input[type="file"]');
    expect(input).not.toBeNull();
    expect(input!.multiple).toBe(true);
    expect(input!.accept).toBe("image/*");
    // 收窄的机器面（§9.1-⑮）：面板源码不接原生拖放三件套
    expect(String(panelSrc)).not.toMatch(/onDrop\b|dataTransfer|onDragDropEvent/);
    const sources = import.meta.glob("../../../**/*.{ts,tsx}", {
      query: "?raw",
      import: "default",
      eager: true,
    }) as Record<string, string>;
    // 测试源码自带这些字面量（就是本枚断言写的），扫描范围取生产源码
    const isProd = (path: string) => !path.includes("__tests__") && !/\.test\.tsx?$/.test(path);
    const allProd = Object.entries(sources).filter(([path]) => isProd(path));
    // 防空洞：扫描确实读到了真源码（本仓生产模块数远大于 10，且能扫到既有命令字面量）
    expect(allProd.length, "全仓 raw 扫描不得是空表").toBeGreaterThan(10);
    expect(
      allProd.filter(([, src]) => src.includes("ocr_recognize")).length,
      "正对照：全仓扫描须命中含 ocr_recognize 的生产源码",
    ).toBeGreaterThan(0);
    const withDrag = allProd.filter(([, src]) => src.includes("onDragDropEvent"));
    // 锚翻正（D-33/T-B8-1，2026-09-25 B8 裁决放行）：窗口级拖放已在 kvm 面板交付，
    // "全仓零命中"收窄判据收缩为"kvm/KvmPanel.tsx 唯一命中"；OCR 面板本身仍不接（上方断言未动）
    expect(withDrag.length, "kvm 拖放交付点恰一枚（防锚点整体失踪）").toBe(1);
    expect(
      withDrag.map(([path]) => path).filter((p) => !p.endsWith("kvm/KvmPanel.tsx")),
      "窗口级拖放限 kvm/KvmPanel.tsx，其余生产源码仍零命中",
    ).toEqual([]);
  });

  it("ocrBatch_cancelMidway_stopsInvoking", async () => {
    const gates = gated(OK_RESULT);
    await mount();
    await pickFiles([
      ["a.png", "A"],
      ["b.png", "B"],
      ["c.png", "C"],
    ]);
    await flush();
    expect(ocrRecognize).toHaveBeenCalledTimes(1);
    await click(buttonsWith("清空队列")[0]);
    gates.shift()?.release(); // 在途那张照常了结
    await flush();
    // 清空 = 作废批次：剩余两张一次都不再送识别
    expect(ocrRecognize).toHaveBeenCalledTimes(1);
    expect(container.textContent).not.toContain("批量队列");
    // 正对照：重新选一张即另起批次（队列不是永久死掉）
    vi.mocked(ocrRecognize).mockImplementation(async () => OK_RESULT);
    await pickFiles([["d.png", "D"]]);
    await flush();
    expect(ocrRecognize).toHaveBeenCalledTimes(2);
    expect(container.textContent).toContain("d.png");
  });
});
