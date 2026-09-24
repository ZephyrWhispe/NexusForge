import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SysPanel from "../SysPanel";
import {
  sysCleanTargets,
  sysKill,
  sysMetricsHistory,
  sysProcesses,
  type ProcessRowDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// D-29 B7/T-B7-10 红线回归：进程页结束钮走「逐字复述进程名」输入确认词闸——
// 复述不符「确认结束」钮不可用（且 confirmAction/sysKill 均零触达），逐字相符才放行。
// 修前：无进程页（无从谈起输入闸）。后端 ProcessTable 同款闸不被 UI 闸代替。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    sysMetricsHistory: vi.fn(),
    sysCleanTargets: vi.fn(),
    sysProcesses: vi.fn(),
    sysKill: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(),
}));

const ROWS: ProcessRowDto[] = [
  { pid: 4321, name: "Notepad.exe", cpu_pct: 12.5, mem_bytes: 20 * 1024 * 1024, disk_bps: null },
  { pid: 4322, name: "chrome.exe", cpu_pct: 30, mem_bytes: 300 * 1024 * 1024, disk_bps: 1024 },
];

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function setInputValue(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function killInput(): HTMLInputElement | undefined {
  return [...document.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith("逐字输入"),
  );
}

/** 两拍装载的真实节拍等待（loadProcs 内 600ms 间隔） */
async function settleBeats() {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 700));
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(sysMetricsHistory).mockResolvedValue([]);
  vi.mocked(sysCleanTargets).mockResolvedValue([]);
  vi.mocked(sysProcesses).mockResolvedValue(ROWS);
  vi.mocked(sysKill).mockResolvedValue("Notepad.exe");
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
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SysPanel />);
  });
  await settleBeats();
}

describe("SysPanel 进程页复述名输入闸（T-B7-10 红线）", () => {
  it("sysPanel_processSection_killDialogRequiresTypedName：复述不符钮不可用零触达，逐字相符才走确认+sysKill", async () => {
    await mount();
    // 进程 Section 在册：两行 + 首轮差值语义如实（disk_bps=null 显 — 不编 0）
    expect(container.textContent).toContain("进程（2）");
    expect(container.textContent).toContain("Notepad.exe");
    expect(container.textContent).toContain("pid 4321");
    expect(container.textContent).toContain("—");

    const killButtons = [...document.querySelectorAll("button")].filter(
      (b) => b.textContent?.trim() === "结束",
    );
    expect(killButtons.length, "每行应有结束钮").toBe(2);
    await click(killButtons[0]!);

    const input = killInput();
    expect(input, "点结束后应展开逐字复述输入框").toBeDefined();
    const confirmBtn = buttonByText("确认结束")!;
    // 红线核心：空输入/部分复述都不放行（confirmAction 与 sysKill 零触达）
    expect(confirmBtn.disabled).toBe(true);
    setInputValue(input!, "note");
    await act(async () => {});
    expect(buttonByText("确认结束")!.disabled, "部分复述不得放行").toBe(true);
    setInputValue(input!, "notepad.exe");
    await act(async () => {});
    // 大小写宽容与后端 eq_ignore_ascii_case 同谱：放行
    expect(buttonByText("确认结束")!.disabled).toBe(false);
    expect(sysKill).not.toHaveBeenCalled();
    expect(confirmAction).not.toHaveBeenCalled();

    await click(buttonByText("确认结束")!);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    const askArg = vi.mocked(confirmAction).mock.calls[0]![0];
    expect(askArg.title).toContain("结束进程");
    // impact 联合类型（string | string[]）走同款归一化，不猜形状
    expect([askArg.impact].flat().join("\n")).toContain("Notepad.exe");
    // 外发 = 逐字复述词 + 目标 pid（回执走 sys_kill(pid, confirmName)）
    expect(sysKill).toHaveBeenCalledTimes(1);
    expect(vi.mocked(sysKill).mock.calls[0]).toEqual([4321, "notepad.exe"]);
    await settleBeats();
  });

  it("sysPanel_processSection_cancelAndSort：取消撤输入闸；排序头重载带 sort/query", async () => {
    await mount();
    const killButtons = [...document.querySelectorAll("button")].filter(
      (b) => b.textContent?.trim() === "结束",
    );
    await click(killButtons[1]!);
    expect(killInput()).toBeDefined();
    await click(buttonByText("取消")!);
    expect(killInput(), "取消后复述闸撤下").toBeUndefined();
    expect(sysKill).not.toHaveBeenCalled();
    // 排序头：点「内存」按 mem 重载；搜索框随刷新携带 query
    await click(buttonByText("内存")!);
    await settleBeats();
    const memCall = vi
      .mocked(sysProcesses)
      .mock.calls
      .find(([sort]) => sort === "mem");
    expect(memCall, "排序头应以 mem 触达 sys_processes").toBeDefined();
  });
});
