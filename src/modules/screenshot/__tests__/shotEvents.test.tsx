/**
 * D-39③④：`screenshot.taken` 门铃刷新 + `ocr.failed` 错误提醒。
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
