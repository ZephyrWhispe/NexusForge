/**
 * D-40：KVM 发现降级上屏。组播不可达（离线笔记本实测 10065）时后端经
 * discovered_peers 返回体带 degraded 原因，空列表是"降级"不是"没设备"——
 * 面板必须把原话亮出来并给出直连不受影响指引；degraded 为 null 时维持
 * 正常空态文案（降级叙述不得无中生有）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import KvmPanel from "../KvmPanel";
import { SUBNAV } from "../../../layout/modules";
import {
  kvmControlState,
  kvmDiscoveredPeers,
  kvmEdgeMap,
  kvmIssuePairCode,
  kvmPairedPeers,
  kvmSessionList,
} from "../../../ipc/client";

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
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.mocked(kvmPairedPeers).mockResolvedValue([]);
  vi.mocked(kvmSessionList).mockResolvedValue([]);
  vi.mocked(kvmControlState).mockResolvedValue({ role: "idle" } as never);
  vi.mocked(kvmEdgeMap).mockResolvedValue({});
  vi.mocked(kvmIssuePairCode).mockResolvedValue(["ABCDEF", 60] as never);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

async function mountWith(degraded: string | null) {
  vi.mocked(kvmDiscoveredPeers).mockResolvedValue({ peers: [], degraded });
  await act(async () => root.render(<KvmPanel />));
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

const DEGRADE_REASON =
  "组播发现不可用（socket 绑定失败：os error 10065（No route to host））";

describe("KvmPanel 发现降级（D-40）", () => {
  it("kvmPanel_discoveryDegraded_showsBackendReason", async () => {
    await mountWith(DEGRADE_REASON);
    const text = bodyText();
    expect(text).toContain("局域网自动发现已降级");
    // 后端原话逐字上屏，不粉饰
    expect(text).toContain(DEGRADE_REASON);
    // 用户最关心的那句：直连能力不受影响
    expect(text).toContain("已配对设备的连接、剪贴板与文件传输不受影响");
    // 降级形态下不得再报"暂未发现"（那会把环境问题说成没设备）
    expect(text).not.toContain("暂未发现未配对设备");
  });

  it("kvmPanel_discoveryHealthy_keepsNormalEmptyState", async () => {
    // 正对照：同一套代码，degraded=null 时空态回到原文案
    await mountWith(null);
    const text = bodyText();
    expect(text).toContain("暂未发现未配对设备");
    expect(text).not.toContain("局域网自动发现已降级");
  });

  it("kvmPanel_anchorMarksMatchRailRegistry_inOrder", async () => {
    // D-43 C8：左轨 anchor 条目的落点判据取真挂载 DOM（layoutCompliance 的源码扫只证字面在场，
    // 不证区块当下真在页上、更不证次序）。次序同判——锚点与左轨行错位＝目录指错页。
    await mountWith(null);
    const marks = [...container.querySelectorAll("[data-nf-sec]")].map(
      (el) => el.getAttribute("data-nf-sec")!,
    );
    expect(marks).toEqual(SUBNAV.kvm.flatMap((s) => s.items).map((i) => i.id));
  });
});
