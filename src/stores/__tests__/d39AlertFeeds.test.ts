import { describe, expect, it, vi } from "vitest";

// D-39①②：两枚主窗口级 feed 的正反回归（kvmPairAlerts.test.ts 同夹具）。
// 注：feed 各有模块级 feedStarted 闩（幂等设计），每枚只 start 一次，
// 正反断言在同一用例内按序进行。

let automationCb: ((e: { payload: unknown }) => void) | null = null;
let crashCb: ((e: { payload: unknown }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, cb: (e: { payload: unknown }) => void) => {
    // 两个 feed 各自 import 时注册一次：按注册顺序分发到对应用例句柄
    if (!automationCb) automationCb = cb;
    else crashCb = cb;
    return () => {};
  }),
}));

vi.mock("../notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

import { notify } from "../notifications";
import { startAutomationNotifyFeed } from "../automationNotifications";
import { startModuleCrashAlertFeed } from "../moduleCrashAlerts";

describe("automationNotify feed（D-39①）", () => {
  it("ruleNotify_toasts_andNoiseStaysSilent", async () => {
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    startAutomationNotifyFeed();
    await vi.waitFor(() => expect(automationCb).toBeTypeOf("function"));

    automationCb!({
      payload: {
        topic: "automation.notify",
        payload: { rule_id: "r1", title: "下载完成", body: "文件已入库" },
      },
    });
    expect(notify).toHaveBeenCalledTimes(1);
    const [kind, title, body] = vi.mocked(notify).mock.calls[0];
    expect(kind).toBe("info");
    expect(title).toBe("下载完成");
    expect(body).toContain("文件已入库");

    // 反例：他域事件、无标题通知零提醒
    automationCb!({ payload: { topic: "automation.rule_fired", payload: { rule_id: "r1" } } });
    automationCb!({ payload: { topic: "automation.notify", payload: { body: "无标题" } } });
    expect(notify).toHaveBeenCalledTimes(1);
  });
});

describe("moduleCrashAlert feed（D-39②）", () => {
  it("moduleCrash_toastsWithReason_andNoiseStaysSilent", async () => {
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    startModuleCrashAlertFeed();
    await vi.waitFor(() => expect(crashCb).toBeTypeOf("function"));

    crashCb!({
      payload: {
        topic: "host.module_crashed",
        payload: { module: "kvm", message: "index out of bounds" },
      },
    });
    const calls = vi.mocked(notify).mock.calls;
    // setup 的 clearMocks 使每用例 mock 计数从零起（跨用例不串计数）
    expect(calls).toHaveLength(1);
    const [kind, title, body] = calls[0];
    expect(kind).toBe("error");
    expect(title).toContain("kvm");
    expect(body).toContain("index out of bounds");

    // 反例：module_state 状态流（红点已覆盖）不该重复弹崩溃提醒
    crashCb!({ payload: { topic: "host.module_state", payload: { module: "kvm", state: "error" } } });
    expect(vi.mocked(notify).mock.calls.length).toBe(1);
  });
});
