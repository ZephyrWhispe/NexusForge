/**
 * 窗口轨的两件判定（09 §9.2 T-B4-4 的"决策半"，§9.1-④ 两半制）。
 *
 * 装配半在 `OverlayShot.tsx`：它在 jsdom 里装载不了（要真 canvas 2d 上下文与 Tauri
 * 窗口 API），因此那边的调用形状由 `__tests__/overlayWindowMode.test.ts` 以 `?raw`
 * 源码扫描钉住；这里只住"给一条任务 → 是不是窗口轨 / 整窗选区是哪一个矩形"这两问，
 * 直接单测。
 */

/** 覆盖层矩形（两套口径同形：CSS 选区与物理裁剪都是左上角 + 宽高，分开命名反而要两个类型） */
export interface OverlayRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/**
 * 这条任务是窗口轨吗？宿主把选中的句柄随 `TaskInfoDto` 带下来（全屏轨为 null/缺键）。
 * `0` 算窗口轨：句柄 0 本身就是无效句柄，交给宿主明说拒绝，不在这里替它改判成全屏。
 */
export function isWindowTask(info: { hwnd?: number | null } | null | undefined): boolean {
  return info?.hwnd != null;
}

/**
 * 整窗选区（**物理**像素，即该窗帧本身）。
 *
 * 刻意不走 `cssToPhysical`：窗口轨的选区恒等于整帧，按帧尺寸算与视口无关；
 * 而覆盖层窗口刚从上一任务的尺寸改过来，这一拍的 `window.innerWidth` 还可能停在旧值
 * （预热窗口复用，尺寸改动与页面重排之间没有同步点）。用它换算会得到一张裁错的图，
 * 而且错得很安静——看起来就像"窗口截出来小了一圈"。
 */
export function wholeFrameCrop(task: { width: number; height: number }): OverlayRect {
  return { x: 0, y: 0, w: task.width, h: task.height };
}

/** 整窗选区的 CSS 口径（回到"重新框选"阶段时把选区铺满可见区，用户看得见当前选了什么） */
export function wholeWindowCss(cssW: number, cssH: number): OverlayRect {
  return { x: 0, y: 0, w: cssW, h: cssH };
}
