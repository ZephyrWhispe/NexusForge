/**
 * D-39③④：`screenshot.taken` 门铃刷新 + `ocr.failed` 错误提醒。
 * D-42 追加 `ocr.completed` 回填腿门铃（识别结果晚于 taken 落地）。
 * 钉两件事：
 * ① taken 事件让历史重取恰一次（门铃形制，historyBell 同谱；他域事件惊不动）；
 * ② ocr.failed 把后端原话升为 error toast（联动 OCR 失败此前静默）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import {
  hostConfigGet,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPins,
  screenshotUploadTargets,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

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
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotUploadTargets: vi.fn(),
  };
});

let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  eventCbs = [];
  vi.mocked(hostConfigGet).mockResolvedValue({});
  vi.mocked(screenshotUploadTargets).mockResolvedValue([]);
  vi.mocked(screenshotHistoryList).mockResolvedValue({ items: [], total: 0, page: 1, size: 30 });
  vi.mocked(screenshotHistoryGet).mockResolvedValue({
    id: "x",
    png_b64: "QUJD",
    format: "image/png",
    annotations: [],
  });
  vi.mocked(screenshotPins).mockResolvedValue([]);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 已卸载 */
    }
  });
  document.body.replaceChildren();
  vi.clearAllMocks();
});

async function mount() {
  const container = document.createElement("div");
  document.body.append(container);
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  await act(async () => {});
  await act(async () => {});
}

async function fire(topic: string, payload: Record<string, unknown> = {}) {
  await act(async () => {
    for (const cb of eventCbs) cb({ payload: { topic, payload } });
  });
}

describe("截图面板 · taken 门铃 + ocr.failed 提醒（D-39③④）", () => {
  it("shotEvents_takenBellRefetchesHistory_once", async () => {
    await mount();
    const before = vi.mocked(screenshotHistoryList).mock.calls.length;
    expect(before, "挂载即应取过一次历史").toBeGreaterThan(0);

    await fire("screenshot.taken", { task_id: "t1", file: "/x/t1.png" });
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(before + 1);

    // 正对照的反面：他域事件不该惊动刷新（门铃只认自己的绳）
    await fire("clipboard.captured", { depth: 1 });
    await fire("screenshot.overlay_requested", { mode: "shot" });
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(before + 1);
  });

  it("shotEvents_ocrCompletedBackfillRefetchesHistory", async () => {
    // D-42：联动 OCR 的结果晚于 taken 才回填历史行的 ocr_text（ocr-core 异步发
    // ocr.completed，截图模块写回）。判据与后端 parse_ocr_backfill 同形：
    // source_task_id 与 text 齐备才算回填腿。
    await mount();
    const before = vi.mocked(screenshotHistoryList).mock.calls.length;
    await fire("ocr.completed", { source_task_id: "t1", text: "识别出的文字", engine: "win-ocr" });
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(before + 1);

    // 反例：ocr_copy_text「复制全部」腿复用同一主题但载荷是 {action:"copied"}，
    // 它不改历史行，跟着重取就是无谓抖动
    await fire("ocr.completed", { action: "copied" });
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(before + 1);
  });

  it("shotEvents_ocrFailed_toastsBackendReason", async () => {
    await mount();
    vi.mocked(notify).mockClear();
    await fire("ocr.failed", { source_task_id: "t9", reason: "引擎未就绪" });
    const calls = vi.mocked(notify).mock.calls;
    expect(calls).toHaveLength(1);
    const [kind, title, body] = calls[0];
    expect(kind).toBe("error");
    expect(title).toContain("OCR");
    expect(body).toContain("引擎未就绪");
  });
});
