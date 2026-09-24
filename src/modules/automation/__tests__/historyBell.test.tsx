import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import RulesPanel from "../RulesPanel";
import { automationDeadLetters, automationPluginsList, automationRulesList, automationRunsGet } from "../../../ipc/client";

// D-29 B7/T-B7-14 回归：rule_fired 事件**只作门铃**——nf:event 回调唯一读取的是
// envelope topic，事实源恒为 automation_runs_get 命令重取（invoke 计数 +1）；
// 非本域事件不得惊动重取（防空洞：回调若无过滤则任何事件都 +1）。

let capturedBell: ((e: { payload: unknown }) => void) | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, cb: (e: { payload: unknown }) => void) => {
    capturedBell = cb;
    return () => {};
  }),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    automationRulesList: vi.fn(),
    automationDeadLetters: vi.fn(),
    automationRunsGet: vi.fn(),
    automationPluginsList: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(),
}));

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  capturedBell = null;
  vi.mocked(automationRulesList).mockResolvedValue([]);
  vi.mocked(automationDeadLetters).mockResolvedValue([]);
  vi.mocked(automationPluginsList).mockResolvedValue([]);
  vi.mocked(automationRunsGet).mockResolvedValue([]);
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
  vi.clearAllMocks();
});

describe("RulesPanel 历史门铃（T-B7-14）", () => {
  it("bell_only_panelRefetches：rule_fired 门铃→automationRunsGet 重取恰 +1；他域事件惊不动", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(<RulesPanel />);
    });
    await act(async () => {});
    expect(capturedBell, "nf:event 门铃订阅必须在场").toBeDefined();
    const before = vi.mocked(automationRunsGet).mock.calls.length;
    await act(async () => {
      capturedBell!({ payload: { topic: "automation.rule_fired" } });
    });
    await act(async () => {});
    expect(vi.mocked(automationRunsGet).mock.calls.length, "门铃必须重取历史").toBe(before + 1);
    // 正对照防空洞：他域事件不得触发重取（回调必须按 topic 过滤）
    const after = vi.mocked(automationRunsGet).mock.calls.length;
    await act(async () => {
      capturedBell!({ payload: { topic: "clipboard.captured" } });
    });
    await act(async () => {});
    expect(vi.mocked(automationRunsGet).mock.calls.length).toBe(after);
    // 历史 Tab 在位（第四枚）
    const runsTab = [...document.querySelectorAll("button")].find((b) =>
      b.textContent?.startsWith("历史"),
    );
    expect(runsTab, "第四 Tab「历史」必须在场").toBeDefined();
  });
});
