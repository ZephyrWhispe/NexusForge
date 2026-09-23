/**
 * 免手输地址（09 §10.2 T-B5-7）：对端 sync 端口由 KVM 心跳宣告，地址在内核一侧解析，
 * 面板从"全局一个手输框"变成"每行看得见在线态与拨号地址"。
 *
 * 三判据：
 * ① **没有全局手输框**（修前 `useState("127.0.0.1:49820")` 是面板默认值 ⇒ 两台设备时
 *    点任何一行的「立即同步」都在往这同一个回环地址发，是假地址的教科书形态）；
 *    行上的「立即同步」一个参数都不填即可跑通（addr 传 null 交内核解析）；
 * ② 手输降级为**行内高级**：默认不渲染输入框，展开才有；收起即撤销那个值（一个看不见
 *    却仍在生效的地址，比没有地址更难查）；填了才优先于发现层，留空一律折成 null
 *    （空串传下去会被当成"手输了一个地址"，解析分支就此绕空）；
 * ③ 离线行显式说"离线"，且**不贡献任何地址字面量**（幻影地址负例）；
 *    `addr_source=false`（本机没接解析器）时在线/离线整列不渲染——那时 `online:false`
 *    的含义是"没查过"，写成"离线"就是撒谎（与 T-B5-4 的"没事实源就没字"同一纪律）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SyncPanel from "../SyncPanel";
import { useSession } from "../../../stores/session";
import {
  syncConflictsGet,
  syncNow,
  syncPeers,
  syncRunsGet,
  syncStatus,
  type PairedPeerDto,
  type SyncPeerStatusDto,
  type SyncStatusDto,
} from "../../../ipc/client";

// 面板订 nf:event（T-B5-8）：jsdom 无 Tauri 事件环，桩成"注册成功、永不触发"
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    syncPeers: vi.fn(),
    syncStatus: vi.fn(),
    syncRunsGet: vi.fn(),
    syncConflictsGet: vi.fn(),
    syncNow: vi.fn(),
  };
});

const ONLINE_ID = "b1b2b3b4-e5f6-7890-abcd-ef1234567890";
const OFFLINE_ID = "c2c2c2c2-c2c2-c2c2-c2c2-c2c2c2c2c2c2";
const SELF_ID = "a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1";
const ONLINE_ADDR = "192.168.1.12:49821";

function peer(id: string, name: string): PairedPeerDto {
  return {
    device_id: id,
    device_name: name,
    fingerprint: "aa:bb:cc:dd",
    pubkey_b64: "AAAA",
    paired_at: Date.parse("2026-09-10T08:00:00"),
  };
}

function peerStatus(id: string, name: string, over: Partial<SyncPeerStatusDto> = {}): SyncPeerStatusDto {
  return {
    device_id: id,
    device_name: name,
    fingerprint: "aa:bb:cc:dd",
    inbound_cursor: 10,
    push_cursor: 20,
    pending_ops: 0,
    last_sync_ms: Date.parse("2026-09-18T10:00:00"),
    last_error: null,
    sync_addr: null,
    online: false,
    ...over,
  };
}

function statusDto(over: Partial<SyncStatusDto> = {}): SyncStatusDto {
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
      peerStatus(ONLINE_ID, "笔记本-B", { sync_addr: ONLINE_ADDR, online: true }),
      peerStatus(OFFLINE_ID, "客厅手机"),
    ],
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  // T-B5-8：本文件测的是**设备行**上的地址读面，那一档住在「设备」子面板里
  useSession.getState().setSyncSubPanel("devices");
  vi.mocked(syncPeers).mockResolvedValue([peer(ONLINE_ID, "笔记本-B"), peer(OFFLINE_ID, "客厅手机")]);
  vi.mocked(syncRunsGet).mockResolvedValue([]);
  vi.mocked(syncConflictsGet).mockResolvedValue([]);
  vi.mocked(syncStatus).mockResolvedValue(statusDto());
  vi.mocked(syncNow).mockResolvedValue({
    pushed: 2,
    pulled_applied: 1,
    pulled_lost: 0,
    conflicts: 0,
  });
});

afterEach(() => {
  if (root) {
    act(() => {
      root?.unmount();
    });
  }
  root = null;
  useSession.getState().setSyncSubPanel("overview");
  container.remove();
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function mount(over: Partial<SyncStatusDto> = {}) {
  vi.mocked(syncStatus).mockResolvedValue(statusDto(over));
  await act(async () => {
    root = createRoot(container);
    root.render(<SyncPanel />);
  });
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

/** 按 DOM 序取第 i 行的某个按钮（行=配对设备行，顺序即 sync_peers 顺序） */
function rowButton(label: string, i: number): HTMLButtonElement {
  const el = [...container.querySelectorAll("button")].filter(
    (b) => (b.textContent ?? "").trim() === label,
  )[i];
  if (!el) throw new Error(`第 ${i} 行没有「${label}」按钮`);
  return el;
}

/** 全文**唯一**的那枚按钮（"收起高级"只可能在展开行上出现一次，多说明就是渲染错了） */
function onlyButton(label: string): HTMLButtonElement {
  const all = [...container.querySelectorAll("button")].filter(
    (b) => (b.textContent ?? "").trim() === label,
  );
  expect(all, `「${label}」应恰好一枚，实际 ${all.length} 枚`).toHaveLength(1);
  return all[0];
}

function badges(text: string): number {
  return [...container.querySelectorAll(".fui-Badge")].filter(
    (b) => (b.textContent ?? "").trim() === text,
  ).length;
}

async function click(el: Element) {
  await act(async () => {
    (el as HTMLButtonElement).click();
  });
  await act(async () => {});
}

async function typeInto(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )?.set;
  await act(async () => {
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** 正文里出现的所有 `a.b.c.d:port` 形状地址（幻影地址扫断言用） */
function addressesInBody(): string[] {
  return bodyText().match(/\d+\.\d+\.\d+\.\d+:\d+/g) ?? [];
}

describe("同步 · 免手输地址（T-B5-7）", () => {
  it("syncDeviceRow_onlineAddrAndSync_noGlobalInput", async () => {
    await mount();
    // 全局手输框：一个都不许有（修前那个预填 127.0.0.1 的框就是本行的靶子）
    expect(container.querySelector("input[placeholder]")).toBeNull();
    expect(bodyText()).not.toContain("127.0.0.1");
    // 地址长在**行上**：在线行有徽标与拨号地址，全文只此一处
    expect(badges("在线")).toBe(1);
    expect(bodyText()).toContain(`拨号地址 ${ONLINE_ADDR}`);
    expect(addressesInBody()).toEqual([ONLINE_ADDR]);

    // 点在线行的「立即同步」：一个地址都不填也走得通（addr 交内核解析 ⇒ 传 null）
    await click(rowButton("立即同步", 0));
    expect(syncNow).toHaveBeenCalledTimes(1);
    expect(vi.mocked(syncNow).mock.calls[0]).toEqual([ONLINE_ID, null]);
    expect(bodyText()).toContain("同步完成：推送 2");
  });

  it("syncPanel_manualAddrOnlyUnderAdvanced", async () => {
    await mount();
    // 默认收起：不存在任何手输框
    expect(container.querySelector("input[placeholder]")).toBeNull();

    // 展开离线行的「高级」（它没有发现层地址 ⇒ placeholder 是通用提示）
    await click(rowButton("高级", 1));
    const box = container.querySelector<HTMLInputElement>(
      'input[placeholder="host:port"]',
    );
    expect(box, "展开高级后才出现手输框").toBeTruthy();

    // 填了就以这里为准
    await typeInto(box!, "10.1.2.3:49999");
    await click(rowButton("立即同步", 1));
    expect(vi.mocked(syncNow).mock.calls[0]).toEqual([OFFLINE_ID, "10.1.2.3:49999"]);

    // 收起即撤销：藏起来却仍生效的地址，比没有地址更难查
    await click(onlyButton("收起高级"));
    expect(container.querySelector("input[placeholder]")).toBeNull();
    await click(rowButton("立即同步", 1));
    expect(vi.mocked(syncNow).mock.calls[1]).toEqual([OFFLINE_ID, null]);

    // 空值臂：展开但什么都不填 ⇒ 传 null，不是空串（空串会被内核当成"手输了一个地址"）
    await click(rowButton("高级", 1));
    await click(rowButton("立即同步", 1));
    expect(vi.mocked(syncNow).mock.calls[2]).toEqual([OFFLINE_ID, null]);
  });

  it("syncDeviceRow_offlinePeer_noPhantomAddr", async () => {
    await mount();
    // 离线行：明说"离线"，且整页只有在线行那一个地址
    expect(badges("离线")).toBe(1);
    expect(bodyText()).toContain("客厅手机");
    expect(addressesInBody()).toEqual([ONLINE_ADDR]);

    // 解析器未接线（addr_source=false）：这两列是"没查过"，一个字都不许出现
    act(() => {
      root?.unmount();
    });
    root = null;
    await mount({ addr_source: false });
    expect(bodyText()).not.toContain("离线");
    expect(bodyText()).not.toContain("在线");
    expect(addressesInBody()).toEqual([]);
  });
});
