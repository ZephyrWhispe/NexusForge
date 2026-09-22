//! 取色器（D-29 B4 T-B4-3）：像素级放大镜与拾色的全部算式住这里，
//! OverlayShot 只做"把鼠标位置交给这些纯函数，再把返回值画出来/写进当前色"。
//!
//! 像素真源是**已提交合成画布**（不是可见画布）：可见画布上有零尺寸的进行中预览，
//! 而取到的色必须与最终导出图上的色逐字节一致——导出的就是那块离屏合成。
//!
//! 通道序契约：`getImageData().data` 是 RGBA，与宿主侧 `util.rs::bgra_to_rgba`
//! 的输出（R 在前、alpha 恒 255）同源；前端不做任何通道交换。

/** 放大镜里每个源像素占的方块边长（整数倍 + 最近邻 = 看到的就是像素本身） */
export const MAG_STEP = 8;
/** 放大镜取样的窗口边长（奇数：正中央那一格就是光标压住的那个像素） */
export const MAG_WINDOW = 15;
/** 放大镜画布边长（15 格 × 每格 8px = 120） */
export const MAG_SIZE = MAG_WINDOW * MAG_STEP;

/** 放大镜窗口的半径（`magnifierSrc` 的 half 入参单点，别让调用方各写一遍 7） */
export function magnifierHalf(): number {
  return (MAG_WINDOW - 1) / 2;
}

/** 放大镜要拷贝的源矩形（画布像素坐标） */
export interface MagSrc {
  sx: number;
  sy: number;
  sw: number;
  sh: number;
}

/**
 * 以 (cx, cy) 为中心取 `2·half+1` 见方的源矩形，越界时**平移**窗口而不是缩小窗口：
 * sw/sh 恒等于请求窗口，贴边时靠边留白就会被放大成一格一格的空，
 * 而"看错邻居像素"比"看不全中心四周"更糟——中心那一格始终是真的那个像素。
 */
export function magnifierSrc(
  cx: number,
  cy: number,
  canvasW: number,
  canvasH: number,
  half: number,
): MagSrc {
  const size = Math.max(1, Math.round(half * 2 + 1));
  const sx = clampOrigin(Math.round(cx - half), size, canvasW);
  const sy = clampOrigin(Math.round(cy - half), size, canvasH);
  return { sx, sy, sw: size, sh: size };
}

function clampOrigin(raw: number, size: number, extent: number): number {
  if (extent <= size) return 0;
  return Math.min(extent - size, Math.max(0, raw));
}

/** RGBA 缓冲里 (x, y) 像素的首通道下标 */
export function pickIndexAt(x: number, y: number, w: number): number {
  return 4 * (y * w + x);
}

/**
 * 读出 `#RRGGBB`（大写十六进制）。越界下标直接抛：把越界读成 `#000000`
 * 等于把"没读到"伪装成"读到黑色"，取色器尤其不该撒这个谎。
 */
export function pixelHexAt(data: Uint8ClampedArray, index: number): string {
  if (!Number.isInteger(index) || index < 0 || index + 2 >= data.length) {
    throw new RangeError(`像素下标越界：index=${index} len=${data.length}`);
  }
  return `#${[data[index], data[index + 1], data[index + 2]].map(byte2).join("")}`;
}

/** 悬停读数：色值 + 十进制三元组（照着读数手打也能复现同一个色） */
export function pixelReadout(data: Uint8ClampedArray, index: number): string {
  const rgb = [data[index], data[index + 1], data[index + 2]].join(", ");
  return `${pixelHexAt(data, index)} · rgb(${rgb})`;
}

function byte2(n: number): string {
  return Math.max(0, Math.min(255, Math.round(n)))
    .toString(16)
    .padStart(2, "0")
    .toUpperCase();
}

/** 放大镜落笔所需的最小画布面（真身是 CanvasRenderingContext2D；测试给记录桩） */
export interface MagnifierCtx {
  imageSmoothingEnabled: boolean;
  clearRect(x: number, y: number, w: number, h: number): void;
  drawImage(
    image: CanvasImageSource,
    sx: number,
    sy: number,
    sw: number,
    sh: number,
    dx: number,
    dy: number,
    dw: number,
    dh: number,
  ): void;
}

/**
 * 把源矩形按最近邻放大到 `size` 见方。关闭平滑必须写在这里、只写在这里：
 * 装配面再表态一次就会出现"某处忘了关"的双线性糊图（放大镜的全部卖点就是不糊）。
 */
export function drawMagnifier(
  ctx: MagnifierCtx,
  source: CanvasImageSource,
  src: MagSrc,
  size: number = MAG_SIZE,
): void {
  ctx.imageSmoothingEnabled = false;
  ctx.clearRect(0, 0, size, size);
  ctx.drawImage(source, src.sx, src.sy, src.sw, src.sh, 0, 0, size, size);
}
