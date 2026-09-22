/**
 * 同步状态读面（09 §10.2 T-B5-4）：面板不许把"启动了"说成"在听"。
 *
 * 三判据：
 * ① 监听徽标跟的是 `listening`（bind 真结果），**端口号在场不构成绿灯**——修前
 *    `监听 :{status.port}` 直读端口，端口被占用时这一行照样绿，是本模块最显眼的假绿；
 * ② 每台配对设备的"还欠几条"来自现读游标（pending>0 才出警示行，0 就一个字都不多写）；
 * ③ `addr_source=false`（本机没接发现层地址解析器）⇒ `sync_addr`/`online` 全是"没查过"，
 *    视图不得据此写"离线"之类的断言（写"离线"和写"在线"同样是撒谎）；接线后的两臂
 *    见 `deviceAddr.test.tsx`。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SyncPanel from "../SyncPanel";
import {
  syncConflictsGet,
  syncPeers,
  syncRunsGet,
  syncStatus,
  type PairedPeerDto,
  type SyncPeerStatusDto,
  type SyncStatusDto,
} from "../../../ipc/client";

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    syncPeers: vi.fn(),
    syncStatus: vi.fn(),
    syncRunsGet: vi.fn(),
    syncConflictsGet: vi.fn(),
  };
});

const PEER_ID = "b1b2b3b4-e5f6-7890-abcd-ef1234567890";
const SELF_ID = "a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1";

function peer(over: Partial<PairedPeerDto> = {}): PairedPeerDto {
  return {
    device_id: PEER_ID,
    device_name: "笔记本-B",
    fingerprint: "aa:bb:cc:dd",
    pubkey_b64: "AAAA",
    paired_at: Date.parse("2026-09-10T08:00:00"),
    ...over,
  };
}

function peerStatus(over: Partial<SyncPeerStatusDto> = {}): SyncPeerStatusDto {
  return {
    device_id: PEER_ID,
    device_name: "笔记本-B",
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

function status(over: Partial<SyncStatusDto> = {}): SyncStatusDto {
  return {
    op_count: 5,
    port: 49820,
    listening: true,
    last_bind_error: null,
    self_device_id: SELF_ID,
    self_name: "台式机-A",
    paused: false,
    auto_sync: false,
    // 本文件的三判据都发生在"没接解析器"的形态下：在线/离线整列因此不得出现一个字
    addr_source: false,
    peers: [peerStatus()],
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(syncPeers).mockResolvedValue([peer()]);
  vi.mocked(syncRunsGet).mockResolvedValue([]);
  vi.mocked(syncConflictsGet).mockResolvedValue([]);
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  container.remove();
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function mountWith(st: SyncStatusDto) {
  vi.mocked(syncStatus).mockResolvedValue(st);
  await act(async () => {
    root = createRoot(container);
    root.render(<SyncPanel />);
  });
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

/** 监听徽标文本：Badge 根节点（`.fui-Badge`）按 "^未?监听 :" 形状精确取它，避免与正文误撞 */
function listenBadge(): string {
  const el = [...container.querySelectorAll(".fui-Badge")].find((s) =>
    /^未?监听 :/.test(s.textContent ?? ""),
  );
  if (!el) throw new Error("没有监听徽标");
  return el.textContent ?? "";
}

function alertBar(): string | null {
  return container.querySelector('[role="alert"]')?.textContent ?? null;
}

/** 警示行相对普通 muted 行的样式分野（"黄字"的机检面：与相邻说明行不同类） */
function warnClassDiffers(lineText: string): boolean {
  const warn = [...container.querySelectorAll("span")].find(
    (s) => s.textContent === lineText,
  );
  const muted = [...container.querySelectorAll("span")].find((s) =>
    s.textContent?.startsWith("配对于"),
  );
  return !!warn && !!muted && warn.className !== muted.className;
}

describe("同步 · 状态读面（T-B5-4）", () => {
  it("syncOverview_listenBadgeRedWhenNotListening", async () => {
    // bind 失败臂：端口号照旧在场（主动同步这条腿仍然可用），但徽标必须是"未监听"
    await mountWith(
      status({
        listening: false,
        last_bind_error: "端口 49820 监听失败：仅允许一种访问权限",
      }),
    );
    expect(syncStatus).toHaveBeenCalledTimes(1);
    expect(listenBadge()).toBe("未监听 :49820");
    expect(alertBar()).toContain("端口 49820 监听失败：仅允许一种访问权限");
    // 解析器未接线（addr_source=false）⇒ 这两列一个字都不许有（"离线"同样是断言）
    expect(bodyText()).not.toContain("离线");
    expect(bodyText()).not.toContain("在线");

    // 正对照防空洞：同一端口 bind 成功 ⇒ 既无红条也不报"未监听"
    act(() => {
      root.unmount();
    });
    await mountWith(status({ listening: true, last_bind_error: null }));
    expect(listenBadge()).toBe("监听 :49820");
    expect(alertBar()).toBeNull();
  });

  it("syncDeviceRow_lagShowsPendingOps", async () => {
    await mountWith(status({ peers: [peerStatus({ pending_ops: 3 })] }));
    expect(bodyText()).toContain("未出账 3 条");
    expect(warnClassDiffers("未出账 3 条")).toBe(true);
    // 上次会话时刻来自流水表（有行才写，无行写"从未同步"）
    expect(bodyText()).toContain("上次同步");

    act(() => {
      root.unmount();
    });
    await mountWith(
      status({ peers: [peerStatus({ pending_ops: 0, last_sync_ms: 0, last_error: null })] }),
    );
    expect(bodyText()).not.toContain("未出账");
    expect(bodyText()).toContain("从未同步");
    expect(bodyText()).not.toContain("上次同步");

    act(() => {
      root.unmount();
    });
    // 失败行同样点名（承重③：进度数字与失败原因同一个读面，不挑好看的显示）
    await mountWith(
      status({ peers: [peerStatus({ last_error: "连接被拒绝" })] }),
    );
    expect(bodyText()).toContain("上次同步");
    expect(bodyText()).toContain("连接被拒绝");
    expect(alertBar()).toContain("上次同步失败：连接被拒绝");
  });
});
