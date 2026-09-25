import { describe, expect, it, vi } from "vitest";

// SEC-11（D-37 R-I1）配对成功强提醒回归：kvm.paired(paired=true) 必弹
// 全局 warn 提醒（含"若非本人操作可解除"指引）；成对噪声——他域事件、
// paired=false 的解除事件——不得惊动提醒。
// 注：feed 有模块级 feedStarted 闩（幂等设计），全文件只 start 一次，
// 正反断言在同一用例内按序进行。

let capturedCb: ((e: { payload: unknown }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, cb: (e: { payload: unknown }) => void) => {
    capturedCb = cb;
    return () => {};
  }),
}));

vi.mock("../notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

import { notify } from "../notifications";
import { startKvmPairAlertFeed } from "../kvmPairAlerts";

describe("kvmPairAlerts feed（D-37 R-I1）", () => {
  it("pairSuccessAlerts_andNoiseStaysSilent", async () => {
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    startKvmPairAlertFeed();
    // feed 经动态 import("@tauri-apps/api/event") 异步注册，先等订阅就位
    await vi.waitFor(() => expect(capturedCb).toBeTypeOf("function"));

    // 正例：配对成功弹 warn 级提醒并点名设备
    capturedCb!({
      payload: {
        topic: "kvm.paired",
        payload: { paired: true, peer: { device_id: "d1", device_name: "客厅笔记本" } },
      },
    });
    expect(notify).toHaveBeenCalledTimes(1);
    const [kind, title, body] = vi.mocked(notify).mock.calls[0];
    expect(kind).toBe("warn");
    expect(title).toContain("配对");
    expect(body).toContain("客厅笔记本");
    expect(body, "必须给出撤销指引").toContain("解除");

    // 反例：解除事件、他域事件零提醒
    capturedCb!({ payload: { topic: "kvm.paired", payload: { paired: false, peer: { device_id: "d1" } } } });
    capturedCb!({ payload: { topic: "kvm.peer_online", payload: { device_id: "d1" } } });
    capturedCb!({ payload: { topic: "clipboard.captured" } });
    expect(notify).toHaveBeenCalledTimes(1);
  });
});
