//! 像素级效果纯函数（D-29 B4 T-B4-2）：模糊的核与两次盒近似遍历住在这里。
//!
//! 为什么不用画布自带的模糊滤镜属性：一是 jsdom 里没有 canvas 后端，测试无从下口；
//! 二是滤镜结果依赖浏览器实现，重放序不再逐位可复现——而标注重放（undo/redo、图层
//! 上下移动后 `replayAll`）要求同一条标注画两次得到同一张图。
//!
//! 与 `gaussianKernel` 的关系：盒式模糊重复两次 ≈ 高斯（一次=三角，两次=分段抛物线），
//! 这是本模块选它做实际遍历算子的原因；核函数单独导出，用于校验"归一化 + 对称"这两条
//! 高斯性质（也是对将来真按核卷积时留下的契约）。

/** 一帧像素缓冲：与 `ImageData` 结构同形（`ctx.getImageData()` 的返回值可直接传入） */
export interface PixelBuffer {
  data: Uint8ClampedArray;
  width: number;
  height: number;
}

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/**
 * 归一化一维高斯核（长度 `2·radius+1`，权重和为 1、首尾对称）。
 *
 * `radius = 0` 返回 `[1]`：恒等核，避免调用方为"零模糊"写特例分支。
 */
export function gaussianKernel(radius: number): number[] {
  const r = Math.max(0, Math.floor(radius));
  if (r === 0) return [1];
  const sigma = Math.max(1, r / 3);
  const size = r * 2 + 1;
  const weights = new Array<number>(size);
  let sum = 0;
  for (let i = 0; i < size; i++) {
    const x = i - r;
    const w = Math.exp(-(x * x) / (2 * sigma * sigma));
    weights[i] = w;
    sum += w;
  }
  return weights.map((w) => w / sum);
}

/** 水平方向盒模糊（边缘像素钳位复制：常量区经此遍历必须逐字节不变） */
function boxBlurH(src: Uint8ClampedArray, dst: Uint8ClampedArray, w: number, h: number, r: number) {
  const span = r * 2 + 1;
  for (let y = 0; y < h; y++) {
    const row = y * w * 4;
    for (let c = 0; c < 4; c++) {
      let sum = 0;
      for (let k = -r; k <= r; k++) sum += src[row + clamp(k, 0, w - 1) * 4 + c];
      for (let x = 0; x < w; x++) {
        dst[row + x * 4 + c] = sum / span;
        sum += src[row + clamp(x + r + 1, 0, w - 1) * 4 + c];
        sum -= src[row + clamp(x - r, 0, w - 1) * 4 + c];
      }
    }
  }
}

/** 垂直方向盒模糊（与 boxBlurH 同一条钳位纪律） */
function boxBlurV(src: Uint8ClampedArray, dst: Uint8ClampedArray, w: number, h: number, r: number) {
  const span = r * 2 + 1;
  for (let x = 0; x < w; x++) {
    const col = x * 4;
    for (let c = 0; c < 4; c++) {
      let sum = 0;
      for (let k = -r; k <= r; k++) sum += src[col + clamp(k, 0, h - 1) * w * 4 + c];
      for (let y = 0; y < h; y++) {
        dst[col + y * w * 4 + c] = sum / span;
        sum += src[col + clamp(y + r + 1, 0, h - 1) * w * 4 + c];
        sum -= src[col + clamp(y - r, 0, h - 1) * w * 4 + c];
      }
    }
  }
}

/**
 * 就地盒模糊：每次 pass = 水平一遍 + 垂直一遍（可分离，代价 O(wh) 与半径无关）。
 *
 * 默认两 pass —— 两次盒近似即高斯近似的取值处；`passes` 显式传参而不是内部写死，
 * 让"再糊一点"是调用方的一个数字而非一次代码改动。
 */
export function applyBoxBlurPass(img: PixelBuffer, radius: number, passes = 2): void {
  const { data, width, height } = img;
  if (width === 0 || height === 0) return;
  const r = Math.max(1, Math.floor(radius));
  const scratch = new Uint8ClampedArray(data.length);
  for (let p = 0; p < Math.max(1, passes); p++) {
    boxBlurH(data, scratch, width, height, r);
    boxBlurV(scratch, data, width, height, r);
  }
}
