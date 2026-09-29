/**
 * D-42：kvm.clip_received 主窗口级提醒。后端早已把对端内容写进本机剪切板并入库
 * （clipboard-core/module.rs:190 起，D-25），UI 侧却无人出声——内容"自己变了"。
 * 反例面同样承重：提醒文案必须不带载荷（剪切板里可能是密码）。
 * 注：feed 有模块级 feedStarted 闩（幂等设计），本档只 start 一次，
 * 正反断言在同一用例内按序进行（d39AlertFeeds.test.ts 同谱）。
 */
import { describe, expect, it, vi } from "vitest";

let clipCb: ((e: { payload: unknown }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, cb: (e: { payload: unknown }) => void) => {
    clipCb = cb;
    return () => {};
  }),
}));

vi.mock("../notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

import { notify } from "../notifications";
import { startKvmClipAlertFeed } from "../kvmClipAlerts";

describe("kvmClipAlert feed（D-42）", () => {
  it("clipReceived_toastsDevice_withoutLeakingPayload_andNoiseStaysSilent", async () => {
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    startKvmClipAlertFeed();
    await vi.waitFor(() => expect(clipCb).toBeTypeOf("function"));

    clipCb!({
      payload: {
        topic: "kvm.clip_received",
        payload: {
          device_id: "dev-7",
          content: { Text: { text: "hunter2-secret", html: null } },
        },
      },
    });
    expect(notify).toHaveBeenCalledTimes(1);
    const [kind, title, body] = vi.mocked(notify).mock.calls[0];
    expect(kind).toBe("info");
    expect(title).toContain("dev-7");
    expect(body).toContain("已写入本机剪贴板");
    // 载荷内容不得出现在提醒的任何一段里
    const rendered = vi
      .mocked(notify)
      .mock.calls.map((c) => c.join("|"))
      .join("");
    expect(rendered).not.toContain("hunter2-secret");

    // 反例：他域事件与空信封不出声（缺 device_id 的 clip_received 仍出声，
    // 落"未知设备"——内容已经进了本机剪切板，瞒不住也不该瞒）
    clipCb!({ payload: { topic: "kvm.paired", payload: { paired: true } } });
    clipCb!({ payload: {} });
    expect(vi.mocked(notify).mock.calls.length).toBe(1);

    clipCb!({ payload: { topic: "kvm.clip_received", payload: {} } });
    expect(vi.mocked(notify).mock.calls.length).toBe(2);
    expect(vi.mocked(notify).mock.calls[1][1]).toContain("未知设备");
  });
});
