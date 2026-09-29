/**
 * 面板内嵌启动器搜索（D-42 二级窗独占能力入主窗）：`desktop_launcher_search` /
 * `desktop_launcher_launch` 两枚包装此前只在独立窗口（Alt+Q）里有用，主面板只给了一枚
 * "重建索引"按钮——不想呼出全局窗就没有下文。
 *
 * 三条判据各钉一处形制：
 * ① 空查询零 invoke（不拿空串打一次后端，独立窗同规）；
 * ② 200ms 防抖——一次输入一串字符只发一趟；
 * ③ 过期响应丢弃（seq 门）：慢的那趟后到也不许覆盖新的结果，否则用户看到的是上一个词的表。
 * 外加「打开」真的把命中项的 id 交回 `desktop_launcher_launch`（打分与计频全在后端，
 * 前端只透传——见 crates/desktop-core/src/index.rs:176 search / :210 计频）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import DesktopPanel from "../DesktopPanel";
import {
  desktopLauncherLaunch,
  desktopLauncherSearch,
  desktopLauncherStatus,
  desktopNoteList,
  desktopTidyPlan,
  desktopTidyStatus,
  hostConfigGet,
  hostConfigSet,
  type DesktopLauncherHitDto,
} from "../../../ipc/client";

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    desktopLauncherLaunch: vi.fn(),
    desktopLauncherReindex: vi.fn(),
    desktopLauncherSearch: vi.fn(),
    desktopLauncherStatus: vi.fn(),
    desktopNoteAdd: vi.fn(),
    desktopNoteDone: vi.fn(),
    desktopNoteList: vi.fn(),
    desktopNoteRemove: vi.fn(),
    desktopTidyApply: vi.fn(),
    desktopTidyPlan: vi.fn(),
    desktopTidyRestore: vi.fn(),
    desktopTidyStatus: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

const hit = (id: string, name: string, score: number): DesktopLauncherHitDto => ({
  id,
  name,
  kind: "app",
  path: `C:\\StartMenu\\${name}.lnk`,
  source: "start-menu",
  score,
});

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(desktopTidyStatus).mockResolvedValue(false);
  vi.mocked(desktopTidyPlan).mockResolvedValue({ groups: [], total: 0 });
  vi.mocked(desktopLauncherStatus).mockResolvedValue([true, 4]);
  vi.mocked(desktopNoteList).mockResolvedValue([]);
  vi.mocked(hostConfigGet).mockResolvedValue({ tidy_map: null });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
  vi.mocked(desktopLauncherSearch).mockResolvedValue([hit("a1", "计算器", 0.87)]);
  vi.mocked(desktopLauncherLaunch).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container.remove();
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<DesktopPanel />);
  });
  await act(async () => {});
}

/** 面板里唯一一枚带该 placeholder 的输入框（随记输入框是另一枚） */
function searchInput(): HTMLInputElement {
  const el = container.querySelector<HTMLInputElement>('input[placeholder^="在面板内搜索"]');
  if (!el) throw new Error("内嵌启动器搜索框未渲染");
  return el;
}

async function type(value: string): Promise<void> {
  const el = searchInput();
  // 走原生 setter：直接赋值会被 React 的 value tracker 记成"没变"，onChange 压根不触发
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  // 跨过 200ms 防抖窗，同时把 setTimeout 与 Promise 微任务冲干净
  await act(async () => {
    await new Promise((r) => setTimeout(r, 260));
  });
}

function bodyText(): string {
  return container.textContent ?? "";
}

describe("桌面面板 · 内嵌启动器搜索（D-42）", () => {
  it("desktopPanel_emptyQueryZeroSearch", async () => {
    await mount();
    expect(desktopLauncherSearch).not.toHaveBeenCalled();
    expect(bodyText()).not.toContain("没有匹配项");
  });

  it("desktopPanel_querySearchesOnceAndOpensHit", async () => {
    await mount();
    await type("计");
    await type("计算");
    expect(vi.mocked(desktopLauncherSearch).mock.calls.map((c) => c[0])).toEqual(["计", "计算"]);
    expect(bodyText()).toContain("计算器");
    expect(bodyText()).toContain("0.87");
    expect(bodyText()).toContain("应用");

    const open = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "打开",
    );
    if (!open) throw new Error("结果行没有「打开」按钮");
    await act(async () => {
      open.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(desktopLauncherLaunch).toHaveBeenCalledWith("a1");
    expect(bodyText()).toContain("已打开「计算器」");
  });

  it("desktopPanel_staleResponseIsDropped", async () => {
    let settleFirst: (hits: DesktopLauncherHitDto[]) => void = () => {};
    vi.mocked(desktopLauncherSearch).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          settleFirst = resolve;
        }),
    );
    vi.mocked(desktopLauncherSearch).mockResolvedValueOnce([hit("b2", "画图等", 0.42)]);

    await mount();
    await type("画"); // 慢腿：挂住不返
    await type("画图"); // 快腿：立刻返回
    expect(bodyText()).toContain("画图等");

    await act(async () => {
      settleFirst([hit("a1", "计算器", 0.99)]);
    });
    await act(async () => {});
    expect(bodyText(), "上一个词的迟到响应不得覆盖新结果").not.toContain("计算器");
    expect(bodyText()).toContain("画图等");
  });
});
