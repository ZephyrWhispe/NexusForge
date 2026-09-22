/**
 * 窗口轨装配（09 §9.2 T-B4-4）：`hwnd` 态零拖框、选区即整窗。
 *
 * §9.1-④ 两半制沿用：判据住纯模块（`overlay/windowMode.ts`，本文件直接调用并逐个正对照）；
 * 装配面 `OverlayShot.tsx` 在 jsdom 里装载不了（要真 canvas 2d 上下文与 Tauri 窗口 API），
 * 因此那边以 `?raw` 源码扫描钉住调用形状——"自动臂与手动臂共用同一个裁剪出口"这种事实
 * 只能从源码上看，断言的是**没有第二条 confirm 通路**，而不是"长这样"。
 */
import { describe, expect, it } from "vitest";

import overlaySrc from "../OverlayShot.tsx?raw";
import { isWindowTask, wholeFrameCrop, wholeWindowCss } from "../overlay/windowMode";

describe("窗口轨判据（纯模块）", () => {
  it("overlayWindowMode_skipsMarqueeAndConfirmsWholeWindow", () => {
    // 判据：带句柄＝窗口轨。旧宿主不带这个键（undefined）＝全屏轨，不是"窗口轨但句柄未知"
    expect(isWindowTask({ hwnd: 4242 })).toBe(true);
    expect(isWindowTask({ hwnd: null })).toBe(false);
    expect(isWindowTask({})).toBe(false);
    expect(isWindowTask(undefined)).toBe(false);
    // 句柄 0 是无效句柄，仍然算"用户点了某个窗口"：明说拒绝的活归宿主，覆盖层不替它改判成全屏
    expect(isWindowTask({ hwnd: 0 })).toBe(true);

    // 整窗裁剪＝帧本身，按帧尺寸算，不读视口（预热窗口刚从别的尺寸改过来）
    expect(wholeFrameCrop({ width: 800, height: 600 })).toEqual({ x: 0, y: 0, w: 800, h: 600 });
    expect(wholeWindowCss(1200, 700)).toEqual({ x: 0, y: 0, w: 1200, h: 700 });

    // 装配面：自动臂存在、一次性、且不另开第二条裁剪通路
    const block = overlaySrc.slice(
      overlaySrc.indexOf("const windowAutoDone"),
      overlaySrc.indexOf("// ---------------- 编辑阶段"),
    );
    expect(block).toContain("isWindowTask(t)");
    expect(block).toContain("stage !== \"select\"");
    expect(block).toContain("windowAutoDone.current === t.task_id");
    expect(block).toContain("setRect(wholeWindowCss(");
    expect(block).toContain("confirmSelection(wholeFrameCrop(");
    // 红线：这个臂里没有拖框事件——窗口轨的"选区"是算出来的，不是等用户拖出来的
    expect(block).not.toContain("pointerdown");
    // 正对照：手动拖框那条路仍在（否则上面"没有拖框"可以是整个文件被删空的假绿）
    expect(overlaySrc).toContain("void confirmSelection()");
    // 全文件只有一个 screenshotConfirm 调用点：自动与手动共用同一个裁剪出口，confirm 恰一次
    expect(overlaySrc.match(/await screenshotConfirm\(/g) ?? []).toHaveLength(1);
  });
});
