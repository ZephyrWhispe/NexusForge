/**
 * 滚动截图手动拼接（09 §9.2 T-B4-5）。
 *
 * §9.1-④ 两半制沿用：状态机与文案住纯模块 `overlay/scrollFlow.ts`（本文件直接驱动它，
 * 拿到的是真实调用序列——"连点两次完成拼接只发一条命令"这件事只能这么测）；装配半
 * `overlay/ScrollStrip.tsx` 是可挂载的展示件（只有文案与两颗钮，不碰 canvas/Tauri），
 * 因此那边用真挂载钉；再往上的 `OverlayShot.tsx` 装载不了，以 `?raw` 源码扫描钉住
 * "这条通路上没有任何自动滚动"。
 *
 * 三枚用例的共同红线：**宁可少给功能，也不拼错**。拼错的长图比"拼不上"坏得多——
 * 前者看起来是一张好图，后者用户当场就知道。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import overlaySrc from "../OverlayShot.tsx?raw";
import ScrollStrip from "../overlay/ScrollStrip";
import {
  SCROLL_APPEND_LABEL,
  SCROLL_DEGRADED_COPY,
  SCROLL_MAX_STEPS,
  createScrollFlow,
  initialScrollState,
  isScrollCapped,
  isScrolling,
  scrollStepLabel,
  scrollStripCopy,
  type ScrollDeps,
  type ScrollState,
} from "../overlay/scrollFlow";
import type { ConfirmRect, ScrollStepDto } from "../../ipc/client";

const RECT: ConfirmRect = { x: 10, y: 20, w: 300, h: 400 };

function step(
  partial: Partial<ScrollStepDto> & Pick<ScrollStepDto, "segments" | "height">,
): ScrollStepDto {
  return { degraded: false, preview_b64: "QUJD", ...partial };
}

function running(over: Partial<ScrollState> = {}): ScrollState {
  return {
    id: "s1",
    steps: 1,
    segments: 1,
    height: 500,
    degraded: false,
    busy: false,
    ...over,
  };
}

let container: HTMLDivElement | undefined;
let root: Root | undefined;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 已在 mountStrip 内卸载过 */
    }
  });
  container?.remove();
  document.body.replaceChildren();
  vi.clearAllMocks();
});

/** 挂载（重复调用先卸上一轮：一个用例会看两次同一条的状态变化） */
async function mountStrip(state: ScrollState) {
  if (root) {
    const prev = root;
    act(() => prev.unmount());
    container?.remove();
    document.body.replaceChildren();
  }
  const el = document.createElement("div");
  container = el;
  document.body.append(el);
  await act(async () => {
    root = createRoot(el);
    root.render(<ScrollStrip state={state} onFinish={() => {}} onDiscard={() => {}} />);
  });
}

function stripText(): string {
  return container?.textContent ?? "";
}

function buttonTexts(): string[] {
  return [...(container?.querySelectorAll("button") ?? [])].map((b) => b.textContent ?? "");
}

function flowWith(over: Partial<ScrollDeps> = {}): ScrollDeps {
  return {
    begin: vi.fn().mockResolvedValue("s1"),
    append: vi.fn().mockResolvedValue(step({ segments: 1, height: 500 })),
    finish: vi.fn().mockResolvedValue({ file: "a.png", pin_id: null }),
    discard: vi.fn().mockResolvedValue(undefined),
    notify: vi.fn(),
    ...over,
  };
}

describe("滚动截图步进条", () => {
  it("overlayScroll_appendAddsSegment_andShowsDegradedCopy", async () => {
    const deps = flowWith({
      append: vi
        .fn()
        .mockResolvedValueOnce(step({ segments: 1, height: 500 }))
        .mockResolvedValueOnce(step({ segments: 2, height: 900, degraded: true })),
    });
    const flow = createScrollFlow(deps);
    expect(isScrolling(flow.get())).toBe(false);

    await flow.start(RECT);
    expect(deps.begin).toHaveBeenCalledWith(RECT);
    // 会话刚开：首帧已就位，段数记为 1（begin 只回 id，计数在这侧兜住）
    expect(flow.get()).toMatchObject({ id: "s1", steps: 0, segments: 1, degraded: false });
    await mountStrip(flow.get());
    expect(stripText()).toContain("首帧已就位");

    await flow.step();
    expect(deps.append).toHaveBeenCalledWith("s1");
    expect(flow.get()).toMatchObject({ steps: 1, segments: 1, height: 500, degraded: false });
    await mountStrip(flow.get());
    expect(stripText()).toContain("滚动页面后");
    // 负对照：没降级就不许吓用户——"分段各存一图"是降级专属文案
    expect(stripText()).not.toContain(SCROLL_DEGRADED_COPY);

    await flow.step();
    expect(deps.append).toHaveBeenCalledTimes(2);
    expect(flow.get()).toMatchObject({ steps: 2, segments: 2, height: 900, degraded: true });
    await mountStrip(flow.get());
    expect(stripText()).toContain(SCROLL_DEGRADED_COPY);
    // 段数与累计高度都摆明：降级不是"偷偷少了一段"
    expect(scrollStripCopy(running({ steps: 2, segments: 2, degraded: true }))).toContain(
      "分 2 段",
    );
  });

  it("overlayScroll_finishInvokesOnce_andClosesStrip", async () => {
    let resolveFinish: () => void = () => {};
    const deps = flowWith({
      finish: vi.fn().mockReturnValue(
        new Promise<void>((r) => {
          resolveFinish = () => r();
        }),
      ),
    });
    const flow = createScrollFlow(deps);
    await flow.start(RECT);
    await flow.step();
    // 正对照：此刻步进条是在的
    expect(isScrolling(flow.get())).toBe(true);

    // 连点两下：第二下发生在第一下的 await 之前
    const first = flow.complete(["save"]);
    const second = flow.complete(["save"]);
    expect(deps.finish).toHaveBeenCalledTimes(1);
    expect(deps.finish).toHaveBeenCalledWith("s1", ["save"]);
    resolveFinish();
    await Promise.all([first, second]);
    // 收束即关条
    expect(isScrolling(flow.get())).toBe(false);
    expect(flow.get()).toEqual(initialScrollState);
    // 收束走 finish，不额外发 discard
    expect(deps.discard).not.toHaveBeenCalled();

    // 失败臂同样只发一次，且错误进的是覆盖层既有那条行内侧（不是步进条，它已经关了）
    const failDeps = flowWith({ finish: vi.fn().mockRejectedValue(new Error("磁盘忙")) });
    const failFlow = createScrollFlow(failDeps);
    await failFlow.start(RECT);
    await expect(failFlow.complete(["save"])).resolves.toBeUndefined();
    expect(failDeps.finish).toHaveBeenCalledTimes(1);
    expect(failDeps.notify).toHaveBeenCalledWith("磁盘忙");
    expect(isScrolling(failFlow.get())).toBe(false);
  });

  it("overlayScroll_noAutoScrollInjection_uiOnlyManualStep", async () => {
    await mountStrip(running());
    // 步进条只有两钮：完成 / 放弃。追加在工具条那颗「滚动」钮上，此外没有第三种动作
    expect(buttonTexts()).toHaveLength(2);
    expect(buttonTexts()).toEqual(["完成拼接", "放弃"]);
    expect(stripText()).toContain("滚动页面后");
    // 正对照：那颗钮的会话中文案确实存在（否则"两钮"可以是整个功能被删空的假绿）
    expect(scrollStepLabel(running())).toBe(SCROLL_APPEND_LABEL);
    expect(scrollStepLabel(initialScrollState)).toBe("滚动");

    // 红线：整条覆盖层通路上没有一种滚动注入
    expect(overlaySrc).not.toContain("dispatchEvent");
    expect(overlaySrc).not.toContain("WheelEvent");
    expect(overlaySrc).not.toContain("scrollBy");
    expect(overlaySrc).not.toContain("scrollTo");
    // 正对照：不注入不等于没功能——追加仍然只挂在点击上
    expect(overlaySrc).toContain("screenshotScrollAppend");
    expect(overlaySrc).toContain("onClick={startOrStepScroll}");
    // 帧数上限在 UI 侧提前收口（宿主那道才是硬门），到顶即 disable
    expect(isScrollCapped(running({ steps: SCROLL_MAX_STEPS }))).toBe(true);
    expect(isScrollCapped(running({ steps: SCROLL_MAX_STEPS - 1 }))).toBe(false);
    expect(scrollStripCopy(running({ steps: SCROLL_MAX_STEPS }))).toContain("20 帧上限");
  });
});
