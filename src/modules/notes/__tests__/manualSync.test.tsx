import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel from "../NotesPanel";
import {
  notesCanvasDirs,
  notesCards,
  notesList,
  notesReviewQueue,
  notesSync,
  type NoteSyncResultDto,
} from "../../../ipc/client";

// D-29 B1/T-B1-10 回归：「手动同步」按钮复用进面板 effect 的 sync→条件 refreshList 路径；
// 与在途 promise 同一去重（busy 连点、与自动 effect 竞速均零第二次 notes_sync invoke）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesCards: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesSync: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const SYNC0: NoteSyncResultDto = { added: 0, updated: 0, removed: 0, total: 0 };

let container: HTMLDivElement;
let root: Root;
// notesSync 队列：每次调用弹出一个可控 deferred，令「在途」窗口可精确操纵
let deferreds: {
  promise: Promise<NoteSyncResultDto>;
  resolve: (r: NoteSyncResultDto) => void;
}[] = [];

function makeDeferred() {
  let resolve!: (r: NoteSyncResultDto) => void;
  const promise = new Promise<NoteSyncResultDto>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function flush() {
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<NotesPanel />);
  });
  await flush();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  deferreds = [makeDeferred(), makeDeferred(), makeDeferred(), makeDeferred()];
  let i = 0;
  vi.mocked(notesSync).mockImplementation(() => deferreds[Math.min(i++, 3)].promise);
  vi.mocked(notesList).mockResolvedValue([]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesCanvasDirs).mockResolvedValue([]);
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

describe("NotesPanel 手动同步（T-B1-10）", () => {
  it("notesSync_manualButton_singleInvokePerClick：连点与自动 effect 均同 promise 去重", async () => {
    await mount();
    // 进面板自动 effect 已发起第 1 次（未 resolve → 与手动路径同池竞速）
    expect(notesSync).toHaveBeenCalledTimes(1);
    // 自动在途期间点手动同步：不新建 invoke，共享同一在途 promise（同 promise 去重负例）
    const busyBtn = buttonByText("手动同步"); // syncBusy 期文案为「同步中…」，此按钮不存在
    expect(busyBtn).toBeUndefined();
    expect(buttonByText("同步中…")!.disabled).toBe(true);
    deferreds[0].resolve(SYNC0);
    await flush();

    const btn = buttonByText("手动同步")!;
    expect(btn.disabled).toBe(false);
    const listCallsBefore = vi.mocked(notesList).mock.calls.length;

    // 连点两下：第 1 下发起（第 2 次 invoke），第 2 下落在同一在途 promise 上——
    // jsdom 对 disabled 按钮仍派发 click，故去重钉在 handler 级 syncInFlight 守卫上
    await click(btn);
    const inFlight = buttonByText("同步中…")!;
    expect(inFlight.disabled).toBe(true);
    await click(inFlight);
    expect(notesSync).toHaveBeenCalledTimes(2);
    expect(notesSync).toHaveBeenNthCalledWith(2);

    deferreds[1].resolve({ added: 1, updated: 0, removed: 0, total: 2 });
    await flush();
    // 手动如实播报非零结果，且 changed>0 触发条件 refreshList（复用自动路径同款语义）
    expect(container.textContent).toContain("同步完成：新增 1 · 更新 0 · 移除 0");
    expect(vi.mocked(notesList).mock.calls.length).toBe(listCallsBefore + 1);
    expect(buttonByText("手动同步")!.disabled).toBe(false);
  });

  it("notesSync_noExternalChange_honestZeroCopy：零改动文案点名 total 且不触发 refreshList", async () => {
    await mount();
    deferreds[0].resolve(SYNC0);
    await flush();
    await click(buttonByText("手动同步")!);
    deferreds[1].resolve(SYNC0);
    await flush();
    expect(container.textContent).toContain("同步完成：无外部改动（索引 0 篇）");
    expect(container.textContent).not.toContain("同步完成：新增");
    // 零改动 → 条件 refreshList 不发（notesList 仅挂载那一次）
    expect(notesList).toHaveBeenCalledTimes(1);
  });
});
