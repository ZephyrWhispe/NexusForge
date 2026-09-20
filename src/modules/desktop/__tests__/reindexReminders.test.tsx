import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import DesktopPanel from "../DesktopPanel";
import {
  desktopLauncherReindex,
  desktopLauncherStatus,
  desktopNoteList,
  desktopTidyPlan,
  desktopTidyStatus,
} from "../../../ipc/client";
import {
  startDesktopRemindFeed,
  useDesktopReminders,
  type RemindDueDto,
} from "../../../stores/desktopReminders";

// D-29 B1/T-B1-6 回归：启动器索引重建（唯一新命令，重放内置动作由 Rust 侧
// launcher_reindex_preserves_registered_actions 守住）+ 提醒缓冲上移到主窗口级 feed。

const listenMock = vi.fn();

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    desktopLauncherStatus: vi.fn(),
    desktopLauncherReindex: vi.fn(),
    desktopTidyStatus: vi.fn(),
    desktopTidyPlan: vi.fn(),
    desktopNoteList: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

// 面板级静态守卫用到的源文件（vite ?raw，与 panels.test.ts 同惯例）
import desktopPanelSrc from "../DesktopPanel.tsx?raw";
import mainWorkbenchSrc from "../../../windows/MainWorkbench.tsx?raw";
import quickPanelSrc from "../../../windows/QuickPanel.tsx?raw";
import remindStoreSrc from "../../../stores/desktopReminders.ts?raw";
import clientSrc from "../../../ipc/client.ts?raw";

const EMPTY_PLAN = { groups: [], total: 0 };
const note = (id: string, content: string): RemindDueDto => ({
  id,
  content,
  remind_at: Date.parse("2026-09-19T09:00:00"),
  tags: ["工作"],
});

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<DesktopPanel />);
  });
  await act(async () => {});
}

async function unmount() {
  await act(async () => {
    root?.unmount();
  });
}

/** 取 feed 注册的 nf:event 回调（模拟后端发布） */
function emitRemindDue(payload: RemindDueDto | null) {
  const call = listenMock.mock.calls.find(
    ([topic]) => topic === "nf:event",
  ) as unknown as [string, (e: { payload: unknown }) => void];
  call[1]({
    payload: { topic: "desktop.remind_due", payload: payload ?? null },
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(desktopTidyStatus).mockResolvedValue(false);
  vi.mocked(desktopTidyPlan).mockResolvedValue(EMPTY_PLAN);
  vi.mocked(desktopLauncherStatus).mockResolvedValue([true, 7]);
  vi.mocked(desktopNoteList).mockResolvedValue([]);
  useDesktopReminders.getState().clearDue();
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

describe("DesktopPanel 重建索引 + 提醒缓冲（T-B1-6）", () => {
  it("launcherReindex_buttonInvokesAndBadgeRefreshes：点重建索引即 invoke，徽标按新 status 刷新", async () => {
    await mount();
    expect(container.textContent).toContain("索引就绪 · 7 条");
    vi.mocked(desktopLauncherReindex).mockResolvedValue(12);
    vi.mocked(desktopLauncherStatus).mockResolvedValue([true, 15]);
    const btn = buttonByText("重建索引");
    expect(btn).toBeDefined();
    await click(btn!);
    expect(desktopLauncherReindex).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("索引就绪 · 15 条");
    // 返回数是 App 条目、徽标是合计，两个数字各归其位
    expect(container.textContent).toContain("应用 12 条");
    // 重建在途期间按钮禁用（busy 门）已由 disabled={busy!==""} 承担，此处验落定后可再点
    expect(buttonByText("重建索引")?.disabled).toBe(false);
  });

  it("dueReminders_feedBuffersEventsAcrossUnmount：面板外事件入缓冲、挂载后横幅可见", async () => {
    // store feed 沿用 modules.ts 的浏览器预览回退守卫，jsdom 下需伪装 Tauri 环境
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    startDesktopRemindFeed();
    startDesktopRemindFeed(); // 幂等：第二次呼起不得重复注册
    await act(async () => {}); // feed 内是动态 import，等微任务落定
    expect(listenMock).toHaveBeenCalledTimes(1);
    expect(listenMock.mock.calls[0][0]).toBe("nf:event");

    // 事件在面板挂载之前到达（旧实现这里就永久丢了）
    emitRemindDue(note("n1", "下午3点 交周报"));
    expect(useDesktopReminders.getState().due.map((d) => d.id)).toEqual(["n1"]);

    await mount();
    expect(container.textContent).toContain("下午3点 交周报");

    // 卸载期间继续缓冲，且不回写已卸载面板
    await unmount();
    emitRemindDue(note("n2", "明天上午 评审"));
    emitRemindDue(null); // 负例：载荷缺 id 不进缓冲
    expect(useDesktopReminders.getState().due.map((d) => d.id)).toEqual(["n2", "n1"]);

    await mount();
    expect(container.textContent).toContain("明天上午 评审");
    expect(container.textContent).toContain("缓冲 2 条");
    await click(buttonByText("知道了")!);
    expect(useDesktopReminders.getState().due.map((d) => d.id)).toEqual(["n1"]);
  });

  it("desktopNotesDue_NOTwired_takesDueDestructiveGuard：take_due 是破坏性消费，前端禁止接线", () => {
    // desktop_notes_due 会把到期随记置 reminded=1，与后端 30s 轮询线程抢消费；
    // 事件缓冲已在主窗口级 feed 接管，任何面板/窗口再调即回归。
    for (const [name, src] of [
      ["DesktopPanel.tsx", desktopPanelSrc],
      ["MainWorkbench.tsx", mainWorkbenchSrc],
      ["QuickPanel.tsx", quickPanelSrc],
      ["stores/desktopReminders.ts", remindStoreSrc],
    ] as const) {
      expect(src, name).not.toMatch(/desktopNotesDue\s*\(/);
    }
    // 正对照：同一正则对 client.ts 的导出定义恰命中一次——证明判据能变红，
    // 且守卫盯的是调用点，不是删掉能力本身。
    expect(clientSrc.match(/desktopNotesDue\s*\(/g)).toHaveLength(1);
  });
});
