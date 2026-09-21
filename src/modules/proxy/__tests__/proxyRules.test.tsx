import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ProxyPanel from "../ProxyPanel";
import {
  proxyRulesGet,
  proxyRulesSet,
  proxyStatus,
  type ProxyRulesV2Dto,
  type ProxyStatusDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { useSession } from "../../../stores/session";
import { MAINLAND_DIRECT_PRESET } from "../panels/RulesSection";

// D-29 B2/T-B2-9 五枚任务书字面回归（09 §5.2）：三目标规则表行操作全链路、
// placeholder 反斜杠字面（缺陷⑪a）、事件刷新不覆写未保存编辑（缺陷⑪b 红线）、
// 大陆直连预设去重合并（取消零 invoke）、表尾兜底行不可删。

const listenMock = vi.fn(
  async (_topic: string, cb: (e: { payload: unknown }) => void) => {
    eventCbs.push(cb);
    return () => {};
  },
);
let eventCbs: ((e: { payload: unknown }) => void)[] = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: [string, (e: { payload: unknown }) => void]) => listenMock(...args),
}));

let rulesFixture: ProxyRulesV2Dto;

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    proxyStatus: vi.fn(),
    proxySubs: vi.fn(async () => []),
    proxyNodes: vi.fn(async () => []),
    proxyRulesGet: vi.fn(async () => rulesFixture),
    proxyRulesSet: vi.fn(async () => {}),
    proxyLogs: vi.fn(async () => []),
    proxyKernelSelect: vi.fn(async () => {}),
    proxyKernelInstall: vi.fn(async () => ({})),
    proxyKernelRestart: vi.fn(async () => {}),
    proxyWintunInstall: vi.fn(async () => {}),
    proxyArtifactInstall: vi.fn(async () => ({})),
    proxySetMode: vi.fn(async () => {}),
    proxyDelayTest: vi.fn(async () => []),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

function statusDto(): ProxyStatusDto {
  return {
    mode: "off",
    kernel_running: false,
    kernel_id: null,
    inbound_port: 7890,
    nodes_total: 0,
    subs_total: 0,
    admin: false,
    wintun_installed: false,
    kernel_installed: false,
    kernel_version: null,
    has_backup: false,
    restored_last_run: false,
    kernel: "sing-box",
    kernels: [],
    artifacts: [],
  };
}

let container: HTMLDivElement;
let root: Root;

const btnWithText = (scope: Element, text: string) =>
  Array.from(scope.querySelectorAll("button")).find((b) => b.textContent?.trim() === text);

async function mount() {
  vi.mocked(proxyStatus).mockResolvedValue(statusDto());
  await act(async () => {
    root = createRoot(container);
    root.render(<ProxyPanel />);
  });
  await act(async () => {});
}

async function gotoRules() {
  await act(async () => {
    useSession.getState().setProxySubPanel("rules");
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

/** 原生 select 赋值（automation/RulesPanel 同款行内编辑器，jsdom 直改 value+change） */
async function setSelect(el: HTMLSelectElement, value: string) {
  await act(async () => {
    el.value = value;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function setInput(el: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype,
      "value",
    )?.set;
    setter?.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const q = (sel: string) => container.querySelector(sel) as HTMLElement | null;

/** 事件驱动的后台刷新：模拟 nf:event 总线回调（缺陷⑪b 的触发源） */
async function fireEvent(topic: string) {
  await act(async () => {
    for (const cb of eventCbs) cb({ payload: { topic } });
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  useSession.getState().setProxySubPanel("overview");
  eventCbs = [];
  rulesFixture = {
    rules: [{ kind: "suffix", pattern: "bilibili.com", target: "direct", enabled: true }],
    final_target: "proxy",
    route_mode: "rule",
  };
  vi.mocked(confirmAction).mockResolvedValue(true);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  useSession.getState().setProxySubPanel("overview");
  vi.clearAllMocks();
});

describe("ProxyPanel 分流规则 v2（T-B2-9 五字面）", () => {
  it("proxyRules_tableThreeTargetEditing：行操作全链路 invoke 形状字面断言", async () => {
    await mount();
    await gotoRules();
    // 新增一行 → 编辑其目标 → 删除（回到单行）→ 改第一行值与目标 → 保存
    await click(btnWithText(container, "新增规则") as Element);
    expect(q('select[aria-label="规则类型 2"]')).toBeTruthy();
    const delBtns = Array.from(container.querySelectorAll("button")).filter(
      (b) => b.textContent?.trim() === "删",
    );
    expect(delBtns).toHaveLength(2);
    await click(delBtns[1]);
    expect(q('select[aria-label="规则类型 2"]')).toBeNull();
    await setInput(q('input[aria-label="规则值 1"]') as HTMLInputElement, "10.0.0.0/8");
    await setSelect(q('select[aria-label="规则目标 1"]') as HTMLSelectElement, "block");
    await setSelect(q('select[aria-label="规则类型 1"]') as HTMLSelectElement, "ip_cidr");
    await click(btnWithText(container, "保存规则") as Element);
    // 全量替换语义：invoke 载荷 = 当前表逐字段字面（route_mode/final 未被触碰原样随行）
    expect(proxyRulesSet).toHaveBeenCalledTimes(1);
    expect(proxyRulesSet).toHaveBeenCalledWith({
      rules: [{ kind: "ip_cidr", pattern: "10.0.0.0/8", target: "block", enabled: true }],
      final_target: "proxy",
      route_mode: "rule",
    });
    // 保存成功后清脏并重拉（下一次事件刷新可覆写=由 keepsUnsavedEdits 对照）
    expect(vi.mocked(proxyStatus).mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("proxyRules_placeholderBackslashLiteral：示例反斜杠两字符字面（缺陷⑪a）", async () => {
    await mount();
    await gotoRules();
    const input = q('input[aria-label="规则值 1"]') as HTMLInputElement;
    expect(input).toBeTruthy();
    // 旧串 "cn\baidu.com"（TS 字面量）里的 \b 是退格控制符；修正后必须是反斜杠+b 两字符
    expect(input.placeholder).toContain("cn\\baidu.com");
    expect(input.placeholder).not.toContain("\u0008");
  });

  it("proxyRules_eventRefresh_keepsUnsavedEdits（红线：dirty 时事件刷新不覆写）", async () => {
    await mount();
    await gotoRules();
    await setInput(q('input[aria-label="规则值 1"]') as HTMLInputElement, "my-unsaved.test");
    // 后台改口（模拟他端保存/重启回放）+ 事件刷新 → 未保存编辑必须原地不动
    rulesFixture = {
      rules: [{ kind: "domain", pattern: "server-side.test", target: "proxy", enabled: true }],
      final_target: "direct",
      route_mode: "global",
    };
    await fireEvent("proxy.state_changed");
    const input = q('input[aria-label="规则值 1"]') as HTMLInputElement;
    expect(input.value).toBe("my-unsaved.test");
    // 保存成功 → 脏位清除 → 后续事件刷新恢复后端覆写（正对照防脏位卡死）
    await click(btnWithText(container, "保存规则") as Element);
    expect(proxyRulesSet).toHaveBeenCalledWith(
      expect.objectContaining({
        rules: [expect.objectContaining({ pattern: "my-unsaved.test" })],
      }),
    );
    await fireEvent("proxy.nodes_changed");
    expect(
      (q('input[aria-label="规则值 1"]') as HTMLInputElement).value,
    ).toBe("server-side.test");
  });

  it("proxyRules_preset_mainlandMergesDeduped：确认→去重合并清单；取消→零 invoke", async () => {
    await mount();
    await gotoRules();
    // 取消臂：连读都不许偷跑（零 invoke 副作用判据 = rulesGet/rulesSet 调用计数不动）
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    const getCalls = vi.mocked(proxyRulesGet).mock.calls.length;
    await click(btnWithText(container, "应用大陆直连预设") as Element);
    expect(proxyRulesSet).not.toHaveBeenCalled();
    expect(vi.mocked(proxyRulesGet).mock.calls.length).toBe(getCalls);
    // 确认臂：bilibili.com 已在场（fixture）→ 去重跳过，追加其余 19 条后缀直连
    await click(btnWithText(container, "应用大陆直连预设") as Element);
    expect(proxyRulesSet).toHaveBeenCalledTimes(1);
    const next = vi.mocked(proxyRulesSet).mock.calls[0][0];
    const directSuffix = next.rules.filter(
      (r) => r.kind === "suffix" && r.target === "direct",
    );
    expect(directSuffix).toHaveLength(MAINLAND_DIRECT_PRESET.length);
    const patterns = directSuffix.map((r) => r.pattern);
    expect(new Set(patterns).size).toBe(patterns.length);
    for (const p of MAINLAND_DIRECT_PRESET) expect(patterns).toContain(p);
    // 预设即保存：invoke 后清脏重拉（夹具后端仍是原始单行=本地合并结果被服务端覆写，
    // 证明脏位已随保存清除而非永久拒收刷新）
    await fireEvent("proxy.state_changed");
    expect(
      (q('input[aria-label="规则值 1"]') as HTMLInputElement).value,
    ).toBe("bilibili.com");
  });

  it("proxyRules_finalFallbackRow_notDeletable：兜底行只换目标无删钮", async () => {
    await mount();
    await gotoRules();
    // 行数 = 规则数 + 1 固定兜底行；删钮数恒等于规则数
    expect(container.querySelectorAll("tbody tr")).toHaveLength(2);
    expect(Array.from(container.querySelectorAll("button")).filter((b) => b.textContent?.trim() === "删")).toHaveLength(1);
    const fallback = q('select[aria-label="兜底目标"]') as HTMLSelectElement;
    expect(fallback).toBeTruthy();
    expect(fallback.value).toBe("proxy");
    await setSelect(fallback, "direct");
    await click(btnWithText(container, "保存规则") as Element);
    expect(proxyRulesSet).toHaveBeenCalledWith(
      expect.objectContaining({ final_target: "direct" }),
    );
  });
});
