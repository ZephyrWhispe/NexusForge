/**
 * 两枚出账开关（09 §10.2 T-B5-6）：`auto_sync` 是**配置**（读-改-写走 `host_config_*`
 * 真源），`paused` 是**运行态位**（走 `sync_set_paused`）。共同的界面红线：
 * **写完回读内核再下结论**——面板不按"我刚写了什么"显示，按 `sync_status()` 回读到的
 * 值显示。被 schema 拒收 / 被 apply 腿判坏的值进得了盘也进不了运行态，此时若按点击
 * 下结论，就是在谎报"已开启"（本模块 T-B5-4 修的是同一类假绿：把"启动了"说成"在听"）。
 *
 * 三判据：
 * ① 开关联动的是**整份展开**的写回（只带 auto_sync 一键 = 拿它抹掉用户的静默窗与
 *    冲突保留窗，`host_config_set` 是替换语义）；
 * ② 内核没采纳时：开关**不翻**、成功提示**不出**、错误行点名"未采纳"（负例不是空断言：
 *    同一颗预算下臂①已证明采纳时确实会翻）；
 * ③ 暂停徽标跟的是 `status.paused` 这一个真值，按钮文案随之翻面；写失败时不吞原因。
 *
 * 另有文案诚实面：`**永不**` 的 markdown 星号此前字面泄漏在"同步范围"段落里（00-spec
 * 判据），渲染文本不得再出现裸 `**`。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SyncPanel from "../SyncPanel";
import {
  hostConfigGet,
  hostConfigSet,
  syncConflictsGet,
  syncNow,
  syncPeers,
  syncRunsGet,
  syncSetPaused,
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
    syncNow: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    syncSetPaused: vi.fn(),
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
    // 本文件测的是两枚出账开关：地址事实源接没接线与它们无关，取"没接"这个保守形态
    addr_source: false,
    peers: [peerStatus()],
    ...over,
  };
}

/** 盘上配置（内核侧可观测状态，随写回整份替换） */
let diskCfg: Record<string, unknown>;
/** 运行态读数（回读的就是它） */
let st: SyncStatusDto;
/** 内核是否采纳本次配置写入（false = apply 腿判坏值 ⇒ 运行态零扰动） */
let adopt: boolean;
let pausedFails: boolean;

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  diskCfg = { quiet_period_ms: 120000, conflict_keep_days: 7 };
  st = statusDto();
  adopt = true;
  pausedFails = false;
  vi.mocked(syncPeers).mockResolvedValue([peer()]);
  vi.mocked(syncRunsGet).mockResolvedValue([]);
  vi.mocked(syncConflictsGet).mockResolvedValue([]);
  vi.mocked(syncNow).mockResolvedValue({
    pushed: 0,
    pulled_applied: 0,
    pulled_lost: 0,
    conflicts: 0,
  });
  vi.mocked(syncStatus).mockImplementation(async () => st);
  vi.mocked(hostConfigGet).mockImplementation(async () => ({ ...diskCfg }));
  vi.mocked(hostConfigSet).mockImplementation(async (_module, values) => {
    const next = values as Record<string, unknown>;
    // 整份替换：盘上就是这次写来的那一份，不是并上去的一份
    diskCfg = { ...next };
    if (adopt) st = { ...st, auto_sync: next.auto_sync === true };
  });
  vi.mocked(syncSetPaused).mockImplementation(async (paused) => {
    if (pausedFails) {
      throw {
        kind: "Module",
        data: { code: "SYNC_DB_001", message: "暂停位写入失败：同步内核未就绪" },
      };
    }
    st = { ...st, paused };
  });
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
  container.remove();
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SyncPanel />);
  });
  await act(async () => {});
}

/** 点击后把回读轮询（最多 5×100ms 真计时器）跑完 */
async function clickAndWait(el: Element | undefined | null) {
  if (!el) throw new Error("目标控件未渲染");
  await act(async () => {
    (el as HTMLElement).click();
  });
  // act 里的 promise 链跑完即代表 rereadStatus 已返回（其内部 await 全在同一个链上）
  await act(async () => {
    await new Promise((r) => setTimeout(r, 700));
  });
}

function autoSwitch(): HTMLInputElement {
  const el =
    container.querySelector<HTMLInputElement>(".fui-Switch input[type='checkbox']") ??
    container.querySelector<HTMLInputElement>('input[type="checkbox"]');
  if (!el) throw new Error("自动同步开关未渲染");
  return el;
}

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

function badgeByText(re: RegExp): string {
  const el = [...container.querySelectorAll(".fui-Badge")].find((s) => re.test(s.textContent ?? ""));
  if (!el) throw new Error(`没有匹配 ${re} 的徽标`);
  return el.textContent ?? "";
}

function alertBar(): string | null {
  return container.querySelector('[role="alert"]')?.textContent ?? null;
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

const savedPayload = () => vi.mocked(hostConfigSet).mock.calls[0]?.[1] as Record<string, unknown>;

describe("同步 · 两枚出账开关（T-B5-6）", () => {
  it("syncOverview_autoSwitchWritesHostConfigAndReReadsStatus", async () => {
    await mount();
    expect(autoSwitch().checked).toBe(false);
    expect(hostConfigSet).not.toHaveBeenCalled();

    await clickAndWait(autoSwitch());

    // 读-改-写：另外两键随本次写回原样带上（缺键会被 ConfigStore 抹成默认值）
    expect(hostConfigSet).toHaveBeenCalledTimes(1);
    expect(vi.mocked(hostConfigSet).mock.calls[0]?.[0]).toBe("sync");
    expect(savedPayload()).toEqual({
      quiet_period_ms: 120000,
      conflict_keep_days: 7,
      auto_sync: true,
    });
    // 写完回读：mount 1 次 + 写后至少 1 次
    expect(vi.mocked(syncStatus).mock.calls.length).toBeGreaterThan(1);
    // 开关跟的是回读到的运行态，不是点击本身
    expect(autoSwitch().checked).toBe(true);
    expect(alertBar()).toBeNull();
    expect(bodyText()).toContain("自动同步已开启");

    // 负例（同一颗预算）：内核没采纳 ⇒ 不翻、不报成功、如实点名"未采纳"
    act(() => {
      try {
        root?.unmount();
      } catch {
        /* 用例内已卸载 */
      }
    });
    root = null;
    adopt = false;
    st = statusDto();
    await mount();
    await clickAndWait(autoSwitch());
    expect(autoSwitch().checked).toBe(false);
    expect(bodyText()).not.toContain("自动同步已开启");
    expect(alertBar()).toContain("内核未采纳");
    expect(alertBar()).toContain("自动同步 关");
  });

  it("syncOverview_pauseBadgeReflectsStatus", async () => {
    await mount();
    expect(badgeByText(/自动出账$/)).toBe("未暂停自动出账");
    expect(syncSetPaused).not.toHaveBeenCalled();

    await clickAndWait(buttonByText("暂停自动出账"));
    expect(syncSetPaused).toHaveBeenCalledWith(true);
    expect(badgeByText(/自动出账$/)).toBe("已暂停自动出账");
    // 文案随之翻面：同一枚按钮现在是恢复入口
    expect(buttonByText("恢复自动出账")).toBeDefined();
    expect(buttonByText("暂停自动出账")).toBeUndefined();
    expect(bodyText()).toContain("恢复后不补跑");

    // 恢复臂：只置位，不补跑（面板侧零 sync_now 调用）
    act(() => {
      try {
        root?.unmount();
      } catch {
        /* 用例内已卸载 */
      }
    });
    root = null;
    st = statusDto({ paused: true });
    await mount();
    expect(badgeByText(/自动出账$/)).toBe("已暂停自动出账");
    await clickAndWait(buttonByText("恢复自动出账"));
    expect(syncSetPaused).toHaveBeenCalledWith(false);
    expect(badgeByText(/自动出账$/)).toBe("未暂停自动出账");
    // 恢复语义是"以后照常"，不是"现在补跑"：面板侧没有、也不该有第二次会话发起
    expect(syncNow).not.toHaveBeenCalled();

    // 写失败臂：原因原文上屏，徽标不跟着点击走
    act(() => {
      try {
        root?.unmount();
      } catch {
        /* 用例内已卸载 */
      }
    });
    root = null;
    pausedFails = true;
    st = statusDto({ paused: false });
    await mount();
    await clickAndWait(buttonByText("暂停自动出账"));
    expect(alertBar()).toContain("暂停位写入失败：同步内核未就绪");
    expect(badgeByText(/自动出账$/)).toBe("未暂停自动出账");
    expect(bodyText()).not.toContain("恢复后不补跑");
  });

  it("syncScopeText_noRawMarkdownAsterisks", async () => {
    await mount();
    const text = bodyText();
    // 修前：密码库那句的强调是 markdown 星号，被当字面量渲染了出来
    expect(text).not.toContain("**");
    expect(text).toContain("密码库条目永不自动同步");
    expect(text).toContain("仅手动导出加密包");
    // 强调改由 UI 层加粗承担：渲染文本里"永不"仍在，且是一枚独立元素
    expect([...container.querySelectorAll("span")].some((s) => s.textContent === "永不")).toBe(
      true,
    );
  });
});
