/**
 * 同步五子面板 + 事件门铃（09 §10.2 T-B5-8）：拆页之后，读面仍然只有一条腿——表。
 *
 * 三判据：
 * ① 五档各自渲各自的标记卡，切档**互不串污**（同屏只有一张标记卡：代理/剪切板的
 *    选择态各占一枚 session 键，syncSubPanel 是第三枚，见 stores/session.ts）；
 * ② 事件只当门铃：`sync.state_changed` 到达后面板**重新去读** `sync_status`（调用次数
 *    增长），而不是把事件负载里的数字当成新读面插进 state——反过来"只信事件"一旦丢
 *    一枚，界面就永久少一行，而那行数据其实一直在盘上（承重⑥的老病灶复发形态）；
 * ③ `client.ts` 的 sync 读面形状静态守卫：`SyncStatusDto` 字段全必填（可选字段 =
 *    面板可以"忘了处理 undefined"），且 `interface PairedPeerDto` 恰一处声明
 *    （承重⑭：两处真源必然漂移）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import clientSrc from "../../../ipc/client.ts?raw";
import SyncPanel from "../SyncPanel";
import { useSession, type SyncSubPanel } from "../../../stores/session";
import {
  syncConflictsGet,
  syncDatasetsGet,
  syncPeers,
  syncRunsGet,
  syncStatus,
  type PairedPeerDto,
  type SyncConflictDto,
  type SyncDatasetDto,
  type SyncStatusDto,
} from "../../../ipc/client";

/** nf:event 回调捕获口（门铃要能按得响） */
let eventCbs: ((e: { payload: unknown }) => void)[] = [];

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
    syncPeers: vi.fn(),
    syncStatus: vi.fn(),
    syncRunsGet: vi.fn(),
    syncConflictsGet: vi.fn(),
    syncDatasetsGet: vi.fn(),
  };
});

const PEER_ID = "b1b2b3b4-e5f6-7890-abcd-ef1234567890";
const SELF_ID = "a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1";

/** 五档各自的标记卡标题（渲染哪一档 = 屏幕上出现哪一枚） */
const MARKERS: Record<SyncSubPanel, string> = {
  overview: "同步状态",
  devices: "配对设备（1）",
  datasets: "同步数据集（2）",
  conflicts: "冲突历史（1）",
  activity: "同步活动（1）",
};

function peer(): PairedPeerDto {
  return {
    device_id: PEER_ID,
    device_name: "笔记本-B",
    fingerprint: "aa:bb:cc:dd",
    pubkey_b64: "AAAA",
    paired_at: Date.parse("2026-09-10T08:00:00"),
  };
}

function statusDto(): SyncStatusDto {
  return {
    op_count: 5,
    port: 49820,
    listening: true,
    last_bind_error: null,
    self_device_id: SELF_ID,
    self_name: "台式机-A",
    paused: false,
    auto_sync: false,
    addr_source: true,
    peers: [
      {
        device_id: PEER_ID,
        device_name: "笔记本-B",
        fingerprint: "aa:bb:cc:dd",
        inbound_cursor: 10,
        push_cursor: 20,
        pending_ops: 0,
        last_sync_ms: Date.parse("2026-09-18T10:00:00"),
        last_error: null,
        sync_addr: "192.168.1.12:49821",
        online: true,
      },
    ],
  };
}

function conflictRow(): SyncConflictDto {
  return {
    conflictId: "c1".repeat(16),
    entity: "note",
    entityId: "想法.md",
    lostTs: Date.parse("2026-09-18T09:00:00"),
    lostDevice: "devB-ffffffff-0000",
    winnerDevice: "devA-11111111-0000",
    winnerTs: Date.parse("2026-09-18T09:30:00"),
    lostValue: { content: "这一版只在远端改过", title: "想法" },
    recordedMs: Date.parse("2026-09-18T10:00:00"),
  };
}

function datasets(): SyncDatasetDto[] {
  return [
    { id: "note", label: "笔记库", attached: true },
    { id: "future", label: "在册未接线样例", attached: false },
  ];
}

let container: HTMLDivElement;
let root: Root | null = null;

function bodyText(): string {
  return document.body.textContent ?? "";
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SyncPanel />);
  });
  await act(async () => {});
}

async function goto(view: SyncSubPanel) {
  await act(async () => {
    useSession.getState().setSyncSubPanel(view);
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  eventCbs = [];
  useSession.getState().setSyncSubPanel("overview");
  vi.mocked(syncPeers).mockResolvedValue([peer()]);
  vi.mocked(syncStatus).mockResolvedValue(statusDto());
  vi.mocked(syncDatasetsGet).mockResolvedValue(datasets());
  vi.mocked(syncConflictsGet).mockResolvedValue([conflictRow()]);
  vi.mocked(syncRunsGet).mockResolvedValue([
    {
      id: 7,
      tsMs: Date.parse("2026-09-18T10:00:00"),
      peer: PEER_ID,
      role: "initiator",
      pushed: 2,
      pulledApplied: 1,
      pulledLost: 0,
      conflicts: 1,
      durationMs: 88,
      error: "连接被拒绝",
    },
  ]);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  root = null;
  useSession.getState().setSyncSubPanel("overview");
  container.remove();
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

/** 五枚标记卡里出现了几枚（切档互不串污的机检面：同时只许一枚在场） */
function markersPresent(): string[] {
  return Object.values(MARKERS).filter((m) => bodyText().includes(m));
}

describe("同步 · 五子面板与事件门铃（T-B5-8）", () => {
  it("subPanel_syncFiveScopesRender", async () => {
    const views: SyncSubPanel[] = ["overview", "devices", "datasets", "conflicts", "activity"];
    for (const v of views) {
      await mount();
      await goto(v);
      expect(bodyText(), `${v} 档缺标记卡`).toContain(MARKERS[v]);
      // 同屏只有一枚标记：代理/剪切板/同步三枚键各管各的，切档不互相覆写
      expect(markersPresent()).toEqual([MARKERS[v]]);
      act(() => {
        root?.unmount();
      });
      root = null;
    }
    // 野值不落 store（setSyncSubPanel 收窄）：渲染侧仍回落到当前有效档
    useSession.getState().setSyncSubPanel("bogus");
    expect(useSession.getState().syncSubPanel).toBe("activity");
  });

  it("syncOverview_eventListenerDoesNotReplaceFetch", async () => {
    await mount();
    expect(eventCbs.length, "nf:event 监听未注册").toBeGreaterThan(0);
    const before = vi.mocked(syncStatus).mock.calls.length;
    const beforeConflicts = vi.mocked(syncConflictsGet).mock.calls.length;
    expect(before).toBe(1);

    // 门铃响了：面板重新读表，而不是拿事件负载当读面（事件里那几个数字一个都不进 state）
    await act(async () => {
      eventCbs[0]({
        payload: {
          topic: "sync.state_changed",
          payload: { pushed: 9999, pulled_applied: 9999, pulled_lost: 9999, conflicts: 9999 },
        },
      });
    });
    await act(async () => {});
    expect(vi.mocked(syncStatus).mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(vi.mocked(syncConflictsGet).mock.calls.length).toBeGreaterThan(beforeConflicts);
    // 屏幕上说的是表里那份账（op_count 5），不是事件里那份（9999）
    expect(bodyText()).toContain("变更记录 5 条");
    expect(bodyText()).not.toContain("9999");

    // 无关 topic 不该惊动读面
    const after = vi.mocked(syncStatus).mock.calls.length;
    await act(async () => {
      eventCbs[0]({ payload: { topic: "proxy.state_changed", payload: {} } });
    });
    await act(async () => {});
    expect(vi.mocked(syncStatus).mock.calls.length).toBe(after);
  });

  it("client_syncStatusDtoFieldsRequiredNoDupPairedPeer", () => {
    // 承重⑭：同一 DTO 两处声明 = 两处真源，早晚漂移
    const dup = clientSrc.match(/interface PairedPeerDto\b/g) ?? [];
    expect(dup).toHaveLength(1);

    const block = clientSrc.match(/export interface SyncStatusDto \{([\s\S]*?)\n\}/);
    expect(block, "SyncStatusDto 声明丢失").toBeTruthy();
    const body = block![1];
    // 必填面：可选标记（`?`）与兜底默认值一律不许出现在读面上
    expect(body).not.toMatch(/\w+\?\s*:/);
    expect(body).not.toMatch(/default|fallback/i);
    // 逐字段点名（少一个键就红，防"悄悄退化回可选"）
    for (const key of [
      "op_count",
      "port",
      "listening",
      "last_bind_error",
      "self_device_id",
      "self_name",
      "paused",
      "auto_sync",
      "addr_source",
      "peers",
    ]) {
      expect(body, `SyncStatusDto 缺字段 ${key}`).toMatch(new RegExp(`^\\s*${key}:`, "m"));
    }
  });
});
