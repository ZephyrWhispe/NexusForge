import type { ConfirmRect, ScrollStepDto } from "../../ipc/client";

/**
 * 滚动截图步进条的状态机（D-29 B4 T-B4-5）。
 *
 * 两个刻意的事实：
 * 1. **步进全部由用户点击驱动**。本模块（以及它所在的整条通路）不调用任何滚动接口、
 *    不派发任何滚轮/键盘事件——自动滚动的判定与注入都 [收窄] 掉之后，"滚过头"和
 *    "把目标应用滚到未知位置"这两类故障就没有发生的余地，代价只是用户多点几下。
 * 2. **帧数上限在 UI 侧提前收口，宿主侧另有一道**。这里 disable 只是让用户少撞一次错误，
 *    真正的上限在 `screenshot-core::scroll::SCROLL_MAX_STEPS`，绕不过去。
 */

/** 与宿主侧 `scroll::SCROLL_MAX_STEPS` 同值：一次会话最多采样的帧数 */
export const SCROLL_MAX_STEPS = 20;

export const SCROLL_BEGIN_LABEL = "滚动";
export const SCROLL_APPEND_LABEL = "已滚一段，点此追加";
export const SCROLL_FINISH_LABEL = "完成拼接";
export const SCROLL_DISCARD_LABEL = "放弃";

/** 任务书（09 §9.2 T-B4-5）字面文案：降级后必须原样出现在提示条上 */
export const SCROLL_DEGRADED_COPY = "未能识别重叠区，将分段各存一图";
export const SCROLL_CAP_COPY = `已达 ${SCROLL_MAX_STEPS} 帧上限，请点「${SCROLL_FINISH_LABEL}」`;

export interface ScrollState {
  /** null = 步进条不显示（还没开，或者已经收束/放弃） */
  id: string | null;
  /** 成功追加的帧数（首帧由 begin 抓走，不计在此） */
  steps: number;
  segments: number;
  height: number;
  degraded: boolean;
  busy: boolean;
}

export const initialScrollState: ScrollState = {
  id: null,
  steps: 0,
  segments: 0,
  height: 0,
  degraded: false,
  busy: false,
};

export function isScrolling(s: ScrollState): boolean {
  return s.id !== null;
}

export function isScrollCapped(s: ScrollState): boolean {
  return s.steps >= SCROLL_MAX_STEPS;
}

/** 工具条那颗钮的文案：没开会话就是"滚动"，会话开着再点它就是"再采一帧" */
export function scrollStepLabel(s: ScrollState): string {
  return isScrolling(s) ? SCROLL_APPEND_LABEL : SCROLL_BEGIN_LABEL;
}

/**
 * 步进条文案。未降级时那句必须含"滚动页面后"——它是这条通路对用户的唯一指令，
 * 也正是"没有自动滚动"这件事的正面表述。
 */
export function scrollStripCopy(s: ScrollState): string {
  let text = "滚动页面后点「已滚一段，点此追加」：相邻两帧由宿主对齐重叠行";
  text += s.steps > 0 ? `（已采 ${s.steps + 1} 帧 · ${s.height}px）` : "（首帧已就位）";
  if (s.segments > 1) text += ` · 当前分 ${s.segments} 段`;
  if (s.degraded) text += ` · ${SCROLL_DEGRADED_COPY}`;
  if (isScrollCapped(s)) text += ` · ${SCROLL_CAP_COPY}`;
  return text;
}

/** Tauri invoke 抛的是对象，String(e) 会变成 "[object Object]"（与覆盖层内同款规范化） */
export function scrollErrorText(e: unknown): string {
  if (e && typeof e === "object") {
    const dto = e as { data?: { message?: string; code?: string } };
    if (dto.data?.message) return `${dto.data.message} (${dto.data.code ?? ""})`;
  }
  return e instanceof Error ? e.message : String(e);
}

export interface ScrollDeps {
  begin(rect: ConfirmRect): Promise<string>;
  append(id: string): Promise<ScrollStepDto>;
  finish(id: string, actions: string[]): Promise<unknown>;
  discard(id: string): Promise<unknown>;
  /** 失败一律走覆盖层既有的行内错误条：会话已不在手上时，步进条本身就没了，错误不能跟着一起消失 */
  notify(message: string): void;
}

export interface ScrollFlow {
  get(): ScrollState;
  subscribe(fn: () => void): () => void;
  start(rect: ConfirmRect): Promise<void>;
  step(): Promise<void>;
  complete(actions: string[]): Promise<void>;
  abort(): Promise<void>;
}

/**
 * 最小状态机 + 依赖注入。之所以做成工厂而不是纯 reducer：`complete` 的"只发一次"
 * 靠的是**在 await 之前**就把 id 清掉——这句顺序性只有拿到真实调用序列才测得出来。
 */
export function createScrollFlow(deps: ScrollDeps): ScrollFlow {
  let state = initialScrollState;
  const listeners = new Set<() => void>();
  const set = (next: ScrollState) => {
    state = next;
    for (const fn of listeners) fn();
  };
  const fail = (e: unknown) => {
    deps.notify(scrollErrorText(e));
  };

  return {
    get: () => state,
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    async start(rect) {
      if (state.id !== null || state.busy) return;
      set({ ...state, busy: true });
      try {
        const id = await deps.begin(rect);
        set({ id, steps: 0, segments: 1, height: 0, degraded: false, busy: false });
      } catch (e) {
        set({ ...state, busy: false });
        fail(e);
      }
    },
    async step() {
      const { id, steps } = state;
      if (!id || state.busy) return;
      set({ ...state, busy: true });
      try {
        const next = await deps.append(id);
        set({
          id,
          steps: steps + 1,
          segments: next.segments,
          height: next.height,
          degraded: next.degraded,
          busy: false,
        });
      } catch (e) {
        // 追加失败时会话还在用户手里（id 保留）：已采的部分照样能点「完成拼接」拿走
        set({ ...state, busy: false });
        fail(e);
      }
    },
    async complete(actions) {
      const id = state.id;
      if (!id || state.busy) return;
      // 先清 id 再 await：连点两次「完成拼接」发不出两次命令
      set({ ...state, id: null, busy: true });
      try {
        await deps.finish(id, actions);
        set(initialScrollState);
      } catch (e) {
        // 宿主是先摘走会话再跑动作的，失败时会话已经没了：这里同样不回填 id
        set(initialScrollState);
        fail(e);
      }
    },
    async abort() {
      const id = state.id;
      set(initialScrollState);
      if (id) await deps.discard(id).catch(fail);
    },
  };
}
