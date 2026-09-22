// 像素级模糊的纯函数判据（D-29 B4 T-B4-2）。
// 任务书把这两枚名字列在 Rust 栏，落地时在 TS 侧执行——理由记在该行 [落地补记]：
// 合成图由前端 canvas 产出（§9.1-①"Rust 仅存档"），宿主侧没有像素遍历的消费点。

import { describe, expect, it } from "vitest";
import { applyBoxBlurPass, gaussianKernel, type PixelBuffer } from "../overlay/pixel";

function makeBuffer(width: number, height: number, at?: (x: number, y: number) => [number, number, number, number]): PixelBuffer {
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const [r, g, b, a] = at ? at(x, y) : [0, 0, 0, 255];
      const i = (y * width + x) * 4;
      data[i] = r;
      data[i + 1] = g;
      data[i + 2] = b;
      data[i + 3] = a;
    }
  }
  return { data, width, height };
}

function channel(img: PixelBuffer, x: number, y: number, c: number): number {
  return img.data[(y * img.width + x) * 4 + c];
}

describe("模糊原语", () => {
  it("gaussianKernel_isNormalizedAndSymmetric", () => {
    for (const radius of [0, 1, 3, 8]) {
      const k = gaussianKernel(radius);
      expect(k).toHaveLength(radius * 2 + 1);
      expect(k.reduce((s, w) => s + w, 0)).toBeCloseTo(1, 10);
      for (let i = 0; i < k.length; i++) expect(k[i]).toBe(k[k.length - 1 - i]);
      expect(k[Math.floor(k.length / 2)]).toBe(Math.max(...k));
    }
    // 正对照防"摊平求平均也能过"的空洞：高斯不是等权核，边缘权重必须小于中心
    const k3 = gaussianKernel(3);
    expect(k3[0]).toBeLessThan(k3[3]);
    // 负数/小数半径按 0 起算（不产生半格核）
    expect(gaussianKernel(-2)).toEqual([1]);
  });

  it("boxBlurPass_flatRegionStaysFlat", () => {
    // 常量区（含边缘）一个字节都不该变：任何"边缘变暗/变亮"都说明窗口越界补了 0
    const flat = makeBuffer(16, 16, () => [20, 30, 40, 255]);
    const before = Uint8ClampedArray.from(flat.data);
    applyBoxBlurPass(flat, 3);
    expect(Array.from(flat.data)).toEqual(Array.from(before));

    // 正对照：单像素脉冲必须被摊开（证明上面的"不变"不是"什么都没做"）
    const img = makeBuffer(9, 9, (x, y) => (x === 4 && y === 4 ? [255, 0, 0, 255] : [0, 0, 0, 255]));
    applyBoxBlurPass(img, 1);
    expect(channel(img, 4, 4, 0)).toBeLessThan(255);
    expect(channel(img, 4, 3, 0)).toBeGreaterThan(0);
    expect(channel(img, 3, 4, 0)).toBeGreaterThan(0);
    let glowing = 0;
    for (let p = 0; p < 81; p++) if (img.data[p * 4] > 0) glowing += 1;
    expect(glowing).toBeGreaterThanOrEqual(9); // 1 个亮斑 → 至少 3×3 的邻域仍在发光

    // 确定性/可重放：同一输入两次遍历必须逐字节相同（标注重放的前提）
    const a = makeBuffer(9, 9, (x, y) => (x === 4 && y === 4 ? [255, 0, 0, 255] : [0, 0, 0, 255]));
    const b = makeBuffer(9, 9, (x, y) => (x === 4 && y === 4 ? [255, 0, 0, 255] : [0, 0, 0, 255]));
    applyBoxBlurPass(a, 2);
    applyBoxBlurPass(b, 2);
    expect(Array.from(a.data)).toEqual(Array.from(b.data));
  });
});
