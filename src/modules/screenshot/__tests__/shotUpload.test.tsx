/**
 * 上传目标轨（09 §9.2 T-B4-9）：面板「截图设置 · 上传」+ 历史行上传钮。
 *
 * 钉三件事：
 * ① 目标表来自后端注册表，默认关时**一个上传钮都不该有**（关掉 means 一个字节都不发）；
 * ② 凭据值只进本次 invoke——它既不在配置里，也不在写配置的那几次 hostConfigSet 里；
 * ③ 后端模块拒收端点时，面板说的是那句原话（"明文 http 到公网一律拒"），不粉饰成"保存成功"。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import {
  hostConfigGet,
  hostConfigSet,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  screenshotUpload,
  screenshotUploadTargets,
  type ShotItemDto,
  type UploadTargetInfoDto,
} from "../../../ipc/client";
import { reportError } from "../../../stores/notifications";

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

/** nf:event 回调收集起来，用例里按需"发布"总线事件（面板对事件的反应就是要钉的东西） */
let eventCbs: ((e: { payload: unknown }) => void)[];
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_topic: string, cb: (e: { payload: unknown }) => void) => {
    eventCbs.push(cb);
    return () => {};
  }),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotUploadTargets: vi.fn(),
    screenshotUpload: vi.fn(),
  };
});

function target(partial: Partial<UploadTargetInfoDto>): UploadTargetInfoDto {
  return {
    id: "http-form",
    label: "HTTP 表单",
    endpoint_display: "https://img.example/",
    enabled: false,
    ...partial,
  };
}

function shot(id: string): ShotItemDto {
  return {
    id,
    created_ms: Date.parse("2026-09-19T10:30:00"),
    width: 1920,
    height: 1080,
    file: `/x/${id}.png`,
    ocr_text: null,
  };
}

/** 配置快照夹具：用例改它，面板读它（写回时是展开基底） */
let cfgFixture: Record<string, unknown>;
function baseCfg(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    post_actions: [],
    save_dir: "",
    upload_enabled: false,
    upload_target: "",
    upload_endpoint: "https://img.example/upload",
    upload_field: "file",
    upload_header_name: "",
    upload_link_template: "",
    upload_copy_link: true,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  eventCbs = [];
  container = document.createElement("div");
  cfgFixture = baseCfg();
  vi.mocked(hostConfigGet).mockImplementation(async () => cfgFixture);
  vi.mocked(hostConfigSet).mockImplementation(async (_module, cfg) => {
    cfgFixture = cfg as Record<string, unknown>;
  });
  vi.mocked(screenshotUploadTargets).mockResolvedValue([target({})]);
  vi.mocked(screenshotUpload).mockResolvedValue("https://img.example/i/shot_a9");
  vi.mocked(screenshotHistoryList).mockResolvedValue({
    items: [shot("a9"), shot("b2")],
    total: 2,
    page: 1,
    size: 30,
  });
  vi.mocked(screenshotHistoryGet).mockResolvedValue({
    id: "x",
    png_b64: "QUJD",
    format: "image/png",
    annotations: [],
  });
  vi.mocked(screenshotPins).mockResolvedValue([]);
  vi.mocked(screenshotPinGet).mockResolvedValue({
    id: "p",
    x: 0,
    y: 0,
    width: 1,
    height: 1,
    zoom: 1,
    opacity: 1,
    png_b64: "AA",
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
  document.body.replaceChildren();
  vi.clearAllMocks();
});

async function mount() {
  container = document.createElement("div");
  document.body.append(container);
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  // 目标表与事件订阅都在各自的微任务里落地，各冲一轮
  await act(async () => {});
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

function buttonByText(needle: string): HTMLButtonElement | undefined {
  return [...document.body.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(needle),
  ) as HTMLButtonElement | undefined;
}

function uploadRowButtons(): HTMLButtonElement[] {
  return [...document.body.querySelectorAll("button")].filter((b) => {
    const t = (b.textContent ?? "").trim();
    return t === "上传" || t.startsWith("上传中");
  }) as HTMLButtonElement[];
}

function inputByPlaceholder(prefix: string): HTMLInputElement {
  const el = container.querySelector<HTMLInputElement>(`input[placeholder^="${prefix}"]`);
  if (!el) throw new Error(`找不到 placeholder 以「${prefix}」开头的输入框`);
  return el;
}

async function type(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** 模拟总线发布（payload 形状与 crates/host-core/src/events.rs 的登记一致） */
async function fire(topic: string, payload: Record<string, unknown>) {
  await act(async () => {
    for (const cb of eventCbs) cb({ payload: { topic, payload } });
  });
}

describe("截图面板 · 上传目标（T-B4-9）", () => {
  it("shotSettings_uploadTarget_pickerReflectsRegistry_andOffByDefault", async () => {
    vi.mocked(screenshotUploadTargets).mockResolvedValue([
      target({}),
      target({ id: "legacy", label: "旧图床", endpoint_display: "https://old.example/" }),
    ]);
    await mount();
    // 注册表里的两枚都要露出来（面板不替后端藏目标：用户看不到就没法选）
    expect(bodyText()).toContain("HTTP 表单 · https://img.example/");
    expect(bodyText()).toContain("旧图床 · https://old.example/");
    // 默认关：状态行明说"未启用"，历史行一枚上传钮也不给
    expect(bodyText()).toContain("未启用");
    expect(uploadRowButtons()).toHaveLength(0);
    unmount();

    // 正对照：同一套代码，启用中的目标会让上传钮出现（否则上面的 0 可以是"永远不出"）
    vi.mocked(screenshotUploadTargets).mockResolvedValue([target({ enabled: true })]);
    await mount();
    expect(bodyText()).toContain("启用中：HTTP 表单");
    expect(uploadRowButtons().length).toBeGreaterThan(0);
  });

  it("shotPanel_uploadRow_requiresHeaderField_andSendsValueOnce", async () => {
    cfgFixture = baseCfg({
      upload_enabled: true,
      upload_target: "http-form",
      upload_header_name: "Authorization",
    });
    vi.mocked(screenshotUploadTargets).mockResolvedValue([target({ enabled: true })]);
    await mount();

    // 端点要求请求头 → 凭据值没给之前这枚钮是死的（点了注定失败的钮不是功能）
    const rows = uploadRowButtons();
    expect(rows.length).toBeGreaterThan(0);
    await act(async () => {
      rows[0].click();
    });
    expect(screenshotUpload).not.toHaveBeenCalled();

    await type(inputByPlaceholder("Authorization 的值"), "Bearer xyz");
    const [first] = uploadRowButtons();
    if (!first) throw new Error("上传钮没渲染出来");
    await act(async () => {
      first.click();
    });
    const invokes = vi.mocked(screenshotUpload).mock.calls;
    expect(invokes).toHaveLength(1);
    expect(invokes[0][0]).toBe("a9");
    expect(invokes[0][1]).toBe("Bearer xyz");
    // 凭据不落盘的行为面证据：写配置的那几次里没有一个携带它
    for (const call of vi.mocked(hostConfigSet).mock.calls) {
      expect(JSON.stringify(call[1] ?? null)).not.toContain("Bearer xyz");
    }
    // 成功后清空：值只属于那一次调用，不该在框里过夜
    expect(inputByPlaceholder("Authorization 的值").value).toBe("");
  });

  it("shotSettings_uploadEndpoint_insecureShowsInlineError", async () => {
    await mount();
    const endpoint = inputByPlaceholder("上传端点");
    await type(endpoint, "http://evil.example/upload");
    const save = buttonByText("保存端点");
    if (!save) throw new Error("保存端点钮没渲染出来");
    await act(async () => {
      save.click();
    });
    // 写盘这一步本身是成功的（schema 不管端点合规），所以面板不能自己判红
    expect(hostConfigSet).toHaveBeenCalledTimes(1);
    expect(JSON.stringify(vi.mocked(hostConfigSet).mock.calls[0][1])).toContain(
      "http://evil.example/upload",
    );

    // 真因从模块那一侧经 host.config_rejected 回来，原文上屏
    const message =
      "上传端点不合规：http://evil.example/upload（明文 http 到公网一律拒，只允许 https 或本机 127.0.0.1 · localhost · ::1）";
    await fire("host.config_rejected", { module: "screenshot", error: message });
    const alert = container.querySelector('[role="alert"]');
    expect(alert?.textContent).toBe(message);

    // 正对照：别的模块被拒不该串到这一页来
    unmount();
    await mount();
    await fire("host.config_rejected", { module: "proxy", error: "内核配置不合法" });
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(bodyText()).not.toContain("内核配置不合法");
  });

  it("上传目标读取失败只进日志不炸面板（红字不该替用户编一个原因）", async () => {
    vi.mocked(screenshotUploadTargets).mockRejectedValue(new Error("acl missing"));
    await mount();
    expect(reportError).toHaveBeenCalled();
    // 读不到目标表 ≠ 目标表是空的：这里保守地按"没有目标"处理，但绝不谎称已启用
    expect(bodyText()).toContain("未启用");
    expect(bodyText()).toContain("先填端点");
    expect(uploadRowButtons()).toHaveLength(0);
  });
});

function unmount() {
  act(() => root.unmount());
  document.body.replaceChildren();
}
