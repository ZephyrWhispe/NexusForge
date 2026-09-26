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
  kvmSetEdgeMap,
  type PairedPeerDto,
} from "../../../ipc/client";

// 任务书（09 §7.2 T-B7-8）字面测试名：边缘 Select 四缘选项齐备（本机左/右/顶/底）。
// 后端白名单同批扩四值（kvm-core set_edge_map），此处钉前端可达性。

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
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
    kvmSetEdgeMap: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const peer = (id: string, name: string): PairedPeerDto => ({
  device_id: id,
  device_name: name,
  fingerprint: `fp-${id}-0123456789`,
  pubkey_b64: "cHVi",
  paired_at: 0,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.mocked(kvmDiscoveredPeers).mockResolvedValue({ peers: [], degraded: null });
  vi.mocked(kvmPairedPeers).mockResolvedValue([peer("dev-b", "台式机B")]);
  vi.mocked(kvmSessionList).mockResolvedValue([]);
  vi.mocked(kvmControlState).mockResolvedValue({ role: "idle" } as never);
  vi.mocked(kvmEdgeMap).mockResolvedValue({});
  vi.mocked(kvmIssuePairCode).mockResolvedValue(["ABCDEF", 60] as never);
  vi.mocked(kvmSetEdgeMap).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("KvmPanel 边缘四扩（T-B7-8）", () => {
  it("kvmPanel_edgeSelect_fourOptions", async () => {
    await act(async () => root.render(<KvmPanel />));
    await act(async () => {});
    const selects = [
      ...container.querySelectorAll("select"),
    ] as HTMLSelectElement[];
    expect(selects.length).toBeGreaterThanOrEqual(1);
    const edge = selects.find((s) =>
      [...s.options].some((o) => o.value === "left" || o.value === "right"),
    );
    expect(edge, "配对设备行须有边缘映射 Select").toBeTruthy();
    const values = [...edge!.options].map((o) => o.value);
    expect(values).toEqual(["", "left", "right", "up", "down"]);
    // 正对照：中文标签逐一对应（防选项只改 value 没改文案）
    const labels = [...edge!.options].map((o) => o.textContent?.trim());
    expect(labels).toEqual(["未设置", "本机左缘", "本机右缘", "本机顶缘", "本机底缘"]);
    // 选顶缘 ⇒ 整表送后端（device→edge 形制不变，值域扩四）
    await act(async () => {
      edge!.value = "up";
      edge!.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(kvmSetEdgeMap).toHaveBeenCalledWith({ "dev-b": "up" });
  });
});
