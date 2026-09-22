//! 取色器（09 §9.2 T-B4-3）：放大镜几何 + 像素读数 + "拾色不产生标注"的红线。
//!
//! 两半制沿用 T-B4-1/2 的纪律（§9.1-④）：几何与读数住纯模块（本文件直接调用），
//! 装配面（OverlayShot 在 jsdom 里装载不了——它要真 canvas 2d 上下文与 Tauri 窗口 API）
//! 以 `?raw` 源码扫描判据，并把"改形状就要改名"的字面调用点一起钉住。

import { describe, expect, it } from "vitest";
import overlaySrc from "../OverlayShot.tsx?raw";
import pickSrc from "../overlay/pick.ts?raw";
import {
  MAG_SIZE,
  MAG_STEP,
  MAG_WINDOW,
  drawMagnifier,
  magnifierHalf,
  magnifierSrc,
  pickIndexAt,
  pixelHexAt,
  pixelReadout,
  type MagnifierCtx,
} from "../overlay/pick";
import {
  ANN_KIND_NAME,
  PICKER_LABEL,
  PICKER_TOOL,
  RENDERERS,
  TOOL_ABBR,
  TOOL_KINDS,
} from "../overlay/annotations";

/** 取 `start` 到首个 `\n  };` 之间的源码（组件内单个箭头函数的切片口径） */
function fnSlice(src: string, start: string): string {
  const i = src.indexOf(start);
  expect(i).toBeGreaterThanOrEqual(0);
  const rest = src.slice(i);
  const j = rest.indexOf("\n  };");
  expect(j).toBeGreaterThan(0);
  return rest.slice(0, j);
}

/** 与 `util.rs::bgra_to_rgba` 同序（R 先、alpha 恒 255）的 2×2 RGBA 夹具 */
function rgba2x2(): Uint8ClampedArray {
  return new Uint8ClampedArray([
    10, 20, 30, 255, 255, 136, 0, 255, 0, 0, 0, 0, 1, 2, 3, 255,
  ]);
}

/** 只记录不绘制的放大镜画布面（jsdom 无 2d 上下文，判据要落在"被怎么调用"上） */
function recordingCtx() {
  const calls: (CanvasImageSource | number)[][] = [];
  const ctx: MagnifierCtx = {
    imageSmoothingEnabled: true,
    clearRect: () => {},
    drawImage: (...args) => void calls.push(args.slice(0, 9) as (CanvasImageSource | number)[]),
  };
  return { ctx, calls };
}

describe("取色器", () => {
  it("overlayPicker_magnifierSrcClampsAtEdges", () => {
    const half = magnifierHalf();
    const W = 800;
    const H = 600;
    const cases: [number, number][] = [
      [0, 0],
      [W - 1, 0],
      [0, H - 1],
      [W - 1, H - 1],
      [W / 2, H / 2],
    ];
    for (const [cx, cy] of cases) {
      const src = magnifierSrc(cx, cy, W, H, half);
      // 窗口尺寸恒等于请求窗口：钳位靠平移，绝不靠裁掉半格（裁了就出现空白格）
      expect(src.sw).toBe(MAG_WINDOW);
      expect(src.sh).toBe(MAG_WINDOW);
      expect(src.sx).toBeGreaterThanOrEqual(0);
      expect(src.sy).toBeGreaterThanOrEqual(0);
      expect(src.sx + src.sw).toBeLessThanOrEqual(W);
      expect(src.sy + src.sh).toBeLessThanOrEqual(H);
    }
    // 正对照：中心不钳（钳了才叫越界），四角贴边平移
    const center = magnifierSrc(400, 300, W, H, half);
    expect([center.sx, center.sy]).toEqual([400 - half, 300 - half]);
    expect(magnifierSrc(0, 0, W, H, half).sx).toBe(0);
    expect(magnifierSrc(W - 1, H - 1, W, H, half).sx).toBe(W - MAG_WINDOW);
    // 窄画布（窗口比画布还大）不平移出负原点
    expect(magnifierSrc(2, 2, 5, 5, half)).toEqual({ sx: 0, sy: 0, sw: MAG_WINDOW, sh: MAG_WINDOW });
  });

  it("overlayPicker_pixelHex_readsRgbaOrder", () => {
    const data = rgba2x2();
    expect(pickIndexAt(1, 0, 2)).toBe(4);
    expect(pixelHexAt(data, pickIndexAt(1, 0, 2))).toBe("#FF8800");
    expect(pixelHexAt(data, pickIndexAt(0, 1, 2))).toBe("#000000");
    // alpha 不参与色值（第 4 通道是 0 也读得出同一串=证只取前三字节）
    expect(pixelReadout(data, pickIndexAt(1, 0, 2))).toBe("#FF8800 · rgb(255, 136, 0)");
    expect(() => pixelHexAt(data, data.length)).toThrow(/越界/);
    expect(() => pixelHexAt(data, -4)).toThrow(/越界/);
  });

  it("overlayPicker_clickSetsColor_andCommitsNothing", () => {
    const data = rgba2x2();
    expect(pixelHexAt(data, pickIndexAt(1, 0, 2))).toBe("#FF8800");
    // 装配半：按下走 pickAt，pickAt 只 setColor —— 无标注、无撤销栈、无 IPC、无像素拷贝
    expect(overlaySrc).toContain("pickAt(p)");
    const pickAt = fnSlice(overlaySrc, "const pickAt = ");
    expect(pickAt).toContain("setColor(");
    expect(pickAt).not.toMatch(/commitAnn|syncStackCounts|setStackCounts|undoRef|redoRef|invoke\(/);
    expect(pickAt).not.toMatch(/drawImage/);
  });

  it("overlayPicker_usesNearestNeighbour", () => {
    // 关闭平滑的赋值全仓恰一处，且在放大镜模块内（装配面再表态一次就会有"某处忘了关"）
    expect(pickSrc.match(/imageSmoothingEnabled = false/g)).toHaveLength(1);
    expect(pickSrc).not.toMatch(/imageSmoothingEnabled = true/);
    expect(overlaySrc).not.toContain("imageSmoothingEnabled");
    const { ctx, calls } = recordingCtx();
    const source = {} as CanvasImageSource;
    drawMagnifier(ctx, source, { sx: 3, sy: 5, sw: MAG_WINDOW, sh: MAG_WINDOW });
    expect(ctx.imageSmoothingEnabled).toBe(false);
    expect(calls).toEqual([[source, 3, 5, MAG_WINDOW, MAG_WINDOW, 0, 0, MAG_SIZE, MAG_SIZE]]);
    // 整数倍放大：每格 MAG_STEP 像素，否则"像素级"名不副实
    expect(MAG_SIZE / MAG_WINDOW).toBe(MAG_STEP);
    expect(MAG_STEP).toBe(8);
  });

  it("overlayPicker_notAnAnnotationKind", () => {
    // 红线：取色是 Tool 侧的伪工具，三张标注表都不收它（收了就等于承认它产出标注）
    expect(TOOL_KINDS).not.toContain(PICKER_TOOL);
    expect(Object.keys(ANN_KIND_NAME)).not.toContain(PICKER_TOOL);
    expect(Object.keys(RENDERERS)).not.toContain(PICKER_TOOL);
    expect(TOOL_ABBR[PICKER_TOOL as keyof typeof TOOL_ABBR]).toBeUndefined();
    expect(PICKER_TOOL).toBe("picker");
    expect(PICKER_LABEL).toBe("取色器");
    // 正对照：真标注工具三张表都在（防"三张表全空"的假通过）
    for (const kind of TOOL_KINDS) {
      expect(ANN_KIND_NAME[kind]).toBeTruthy();
      expect(RENDERERS[kind]).toBeTruthy();
    }
    expect(overlaySrc).toContain("PICKER_TOOL");
  });

  it("overlayPicker_hoverReadout_andToolbar", () => {
    // 悬停读数与放大镜浮层住装配面：读数由纯模块 pixelReadout 产出，工具条按钮独立于 TOOL_KINDS
    const hover = fnSlice(overlaySrc, "const readPixel = ");
    expect(hover).toContain("getImageData(");
    expect(hover).toContain("pixelReadout(");
    expect(overlaySrc).toContain("rememberPicked({ x: p.x, y: p.y, hex: hit.hex, readout: hit.readout })");
    expect(overlaySrc).toContain("{picked.readout}");
    expect(overlaySrc).toContain("onClick={() => void copyPickedColor()}");
    expect(overlaySrc).toContain("复制色值");
    // 放大镜只在取色态且有悬停点时出现（凭空浮层=挡住底下的图）
    expect(overlaySrc).toContain("tool === PICKER_TOOL && picked");
    expect(overlaySrc).toContain("MAG_SIZE");
  });
});
