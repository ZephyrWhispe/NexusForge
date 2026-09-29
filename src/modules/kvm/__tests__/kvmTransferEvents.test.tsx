/**
 * D-42：KVM 面板逐主题分派。此前 `topic.startsWith("kvm.")` 的粗门铃把逐文件
 * 进度整吞（既不上屏也无 refresh 价值），并把断会话原因留在事件里无人显影。
 * 本夹具钉两件事：①进度流只更新本地行、不得触发五路重取；②closed 腿的 reason 上屏。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import KvmPanel from "../KvmPanel";
import {
  kvmControlState,
  kvmDiscoveredPeers,
  kvmEdgeMap,
  kvmIssuePairCode,
  kvmPairedPeers,
  kvmSessionList,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

let eventCb: ((e: { payload: unknown }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, cb: (e: { payload: unknown }) => void) => {
    eventCb = cb;
    return () => {};
  }),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    kvmDiscoveredPeers: vi.fn(),
    kvmPairedPeers: vi.fn(),
    kvmSessionList: vi.fn(),
    kvmControlState: vi.fn(),
    kvmEdgeMap: vi.fn(),
    kvmIssuePairCode: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  eventCb = null;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.mocked(kvmDiscoveredPeers).mockResolvedValue({ peers: [], degraded: null });
  vi.mocked(kvmPairedPeers).mockResolvedValue([
    { device_id: "dev-1", device_name: "笔记本", fingerprint: "fp", pubkey_b64: "" } as never,
  ]);
  vi.mocked(kvmSessionList).mockResolvedValue([]);
  vi.mocked(kvmControlState).mockResolvedValue({ role: "idle" } as never);
  vi.mocked(kvmEdgeMap).mockResolvedValue({});
  vi.mocked(kvmIssuePairCode).mockResolvedValue(["ABCDEF", 60] as never);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

async function mount() {
  await act(async () => root.render(<KvmPanel />));
  await act(async () => {});
}

const fire = (topic: string, payload: Record<string, unknown> = {}) => {
  act(() => {
    eventCb!({ payload: { topic, payload } });
  });
};

function bodyText(): string {
  return document.body.textContent ?? "";
}

describe("KvmPanel 逐主题事件分派（D-42）", () => {
  it("fileProgress_rendersRowWithoutRefetchIpc", async () => {
    await mount();
    expect(eventCb).toBeTypeOf("function");
    const before = vi.mocked(kvmSessionList).mock.calls.length;

    fire("kvm.file_progress", { transfer_id: "a1b2c3d4e5", sent_chunks: 3, total_chunks: 12 });
    expect(bodyText()).toContain("3/12");
    expect(bodyText()).toContain("传输 a1b2c3d4");
    // 200ms 合并流不得变成每帧一轮五路 IPC
    expect(vi.mocked(kvmSessionList).mock.calls.length).toBe(before);
    expect(bodyText()).toContain("实时（非持久记录）");
  });

  it("fileIncoming_showsNameAndDevice", async () => {
    await mount();
    fire("kvm.file_incoming", {
      device_id: "dev-1",
      transfer_id: "t2",
      name: "季度报告.pdf",
      size: 4096,
      received: 1,
      total_chunks: 4,
    });
    expect(bodyText()).toContain("季度报告.pdf");
    expect(bodyText()).toContain("1/4");
    // 设备名取自已配对列表，不是裸 device_id
    expect(bodyText()).toContain("笔记本");
  });

  it("transferAckFailureWithoutId_isNotSilent", async () => {
    await mount();
    const before = vi.mocked(kvmSessionList).mock.calls.length;
    fire("kvm.transfer_ack", { transfer_id: null, ok: false, error: "设备 dev-1 无活跃会话" });
    expect(bodyText()).toContain("文件发送失败");
    expect(bodyText()).toContain("失败");
    // 回执腿是终态：门铃照常（与进度腿不同，进度腿那条测试断言"次数不变"）
    expect(vi.mocked(kvmSessionList).mock.calls.length).toBe(before + 1);
  });

  it("sessionClosed_toastsBackendReason", async () => {
    await mount();
    fire("kvm.session_state", { device_id: "dev-1", state: "closed", reason: "对端主动断开" });
    expect(notify).toHaveBeenCalledTimes(1);
    const [kind, title, body] = vi.mocked(notify).mock.calls[0];
    expect(kind).toBe("warn");
    expect(title).toContain("笔记本");
    expect(title).toContain("已断开");
    expect(body).toContain("对端主动断开");
  });

  it("sessionEstablished_doesNotToastClosedNarrative", async () => {
    // 反例：established 臂不带 reason（session.rs:57 的 Established 变体），
    // 不得被当成"断开"播报
    await mount();
    fire("kvm.session_state", { device_id: "dev-1", state: "established" });
    expect(notify).not.toHaveBeenCalled();
    expect(bodyText()).not.toContain("已断开");
  });

  it("nonKvmTopic_isIgnored", async () => {
    await mount();
    const before = vi.mocked(kvmSessionList).mock.calls.length;
    fire("clipboard.captured", { id: 7 });
    fire("host.module_state", { module: "kvm", state: "running" });
    expect(vi.mocked(kvmSessionList).mock.calls.length).toBe(before);
  });
});
