// 任务书字面判据集：标注图层的 z 序 / 命中 / 位移（D-29 B4 T-B4-1，§9.1-④"算法只住纯模块"）。
// 本文件刻意不装载 OverlayShot 组件：edit 阶段在装载期就要 canvas 2d 上下文与 Tauri 窗口 API，
// jsdom 给不了真上下文——图层算法因此外提在 overlay/annotations.ts 里，本处按名直测；
// 装配面（面板逐行铺四钮）由 ?raw 读源码判，不降级为实启冒烟（人工项）。

import { describe, expect, it } from "vitest";
import {
  LAYER_OPS,
  boundsOf,
  hitTest,
  layerRows,
  moveLayer,
  nextLayer,
  normalizeLayers,
  removeAt,
  sortByLayer,
  toggleLock,
  translate,
  type Ann,
} from "../overlay/annotations";
import overlaySrc from "../OverlayShot.tsx?raw";

/** 造一条标注：只给图层语义相关的字段，其余按 rect  defaults */
function ann(kind: Ann["kind"], layer: number, opts: Partial<Ann> = {}): Ann {
  return {
    kind,
    color: "#ff4d4f",
    width: 4,
    points: [
      [0, 0],
      [10, 10],
    ],
    layer,
    locked: false,
    alpha: 1,
    fill: false,
    ...opts,
  };
}

describe("图层 z 序", () => {
  it("overlayAnn_moveLayer_swapsZOrderOnly", () => {
    const a = ann("rect", 0);
    const b = ann("ellipse", 1);
    const c = ann("pen", 2);
    const next = moveLayer([a, b, c], 0, 1);
    // 数组下标一律不动（面板行的物理位置由 layer 决定，不是由数组序决定）
    expect(next.map((x) => x.kind)).toEqual(["rect", "ellipse", "pen"]);
    // 纯函数：入参表原样（就地改 annsRef 会让撤销栈与面板看到两份不同的历史）
    expect([a.layer, b.layer, c.layer]).toEqual([0, 1, 2]);
    // 绘制序变成 椭圆 → 矩形 → 画笔；重编号恒稠密 0..n-1
    expect(sortByLayer(next).map((x) => x.kind)).toEqual(["ellipse", "rect", "pen"]);
    expect(next.map((x) => x.layer)).toEqual([1, 0, 2]);
    // 正对照（否则"上移到底不动"可以是永远不动）：中间那条上移一次得到另一种次序
    const mid = moveLayer([a, b, c], 1, 1);
    expect(mid.map((x) => x.layer)).toEqual([0, 2, 1]);
    expect(sortByLayer(mid).map((x) => x.kind)).toEqual(["rect", "pen", "ellipse"]);
  });

  it("overlayAnn_moveLayer_atEnds_isNoOp", () => {
    const list = [ann("rect", 0), ann("ellipse", 1)];
    expect(moveLayer(list, 0, -1).map((x) => x.layer)).toEqual([0, 1]);
    expect(moveLayer(list, 1, 1).map((x) => x.layer)).toEqual([0, 1]);
    expect(moveLayer(list, 9, 1).map((x) => x.layer)).toEqual([0, 1]);
  });

  it("overlayAnn_normalizeAndNextLayer", () => {
    // 旧历史条目（无 layer/locked 两键）按下标补齐；已有值不覆写
    const legacy = [
      { ...ann("rect", 7), layer: undefined as unknown as number, locked: undefined as unknown as boolean },
      ann("pen", 3),
    ];
    const fixed = normalizeLayers(legacy);
    expect(fixed.map((x) => x.layer)).toEqual([0, 3]);
    expect(fixed.map((x) => x.locked)).toEqual([false, false]);
    expect(nextLayer(fixed)).toBe(4);
    expect(nextLayer([])).toBe(0);
  });

  it("overlayAnn_lockedStillDraws", () => {
    const list = [ann("rect", 0), toggleLock([ann("ellipse", 1)], 0)[0], ann("pen", 2)];
    expect(list[1].locked).toBe(true);
    // 锁定不减项：重放表长度与内容都必须原样在（锁定不是"隐藏"）
    const drawn = sortByLayer(list);
    expect(drawn).toHaveLength(3);
    expect(drawn.map((x) => x.kind)).toEqual(["rect", "ellipse", "pen"]);
    expect(removeAt(list, 1)).toHaveLength(2);
  });
});

describe("命中与位移", () => {
  // 两条同心矩形：外层 layer 0，内层 layer 5；中心点同时落在两者盒内
  const outer = ann("rect", 0, { points: [[0, 0], [100, 100]] });
  const inner = ann("rect", 5, { points: [[30, 30], [70, 70]] });
  const list = [outer, inner];

  it("overlayAnn_hitTest_topMostWins_andSkipsLocked", () => {
    const size: [number, number] = [200, 200];
    const pt: [number, number] = [50, 50];
    // 默认（未锁）：最上层先中
    expect(hitTest(list, pt, size, { skipLocked: true })).toBe(1);
    // 锁上最上层：穿透到下层，而不是"整个不可点"
    const lockedTop = [outer, toggleLock([inner], 0)[0]];
    expect(hitTest(lockedTop, pt, size, { skipLocked: true })).toBe(0);
    // 正对照：不穿透时锁上仍命中最上层（否则上一条断言可以是"永远命中 0"的空洞）
    expect(hitTest(lockedTop, pt, size, { skipLocked: false })).toBe(1);
    // 盒外：null，而不是就近返回
    expect(hitTest(list, [190, 190], size, { skipLocked: true })).toBeNull();
  });

  it("overlayAnn_translate_movesAllPoints", () => {
    const pen = ann("pen", 0, { points: [[1, 2], [3, 4], [5, 6]] });
    const moved = translate(pen, 10, -2);
    expect(moved.points).toEqual([
      [11, 0],
      [13, 2],
      [15, 4],
    ]);
    expect(pen.points[0]).toEqual([1, 2]); // 纯函数：入参不被就地改
    expect(moved.layer).toBe(0);
    // text 保文、number 保序号（位移只动几何，不动内容）
    expect(translate(ann("text", 0, { text: "你好" }), 1, 1).text).toBe("你好");
    expect(translate(ann("number", 0, { seq: 7 }), 1, 1).seq).toBe(7);
  });

  it("overlayAnn_boundsPerKind", () => {
    expect(boundsOf(ann("rect", 0, { points: [[8, 6], [2, 20]] }))).toEqual({
      x: 2,
      y: 6,
      w: 6,
      h: 14,
    });
    expect(boundsOf(ann("pen", 0, { points: [[0, 0], [4, 1], [9, 7]] }))).toEqual({
      x: 0,
      y: 0,
      w: 9,
      h: 7,
    });
    // text/number 由锚点张盒（字号式与绘制侧同源：width*6+8）
    expect(boundsOf(ann("text", 0, { points: [[10, 10]], text: "abc", width: 2 }))).toEqual({
      x: 10,
      y: 10,
      w: 3 * 20 * 0.6,
      h: 20,
    });
    expect(boundsOf(ann("number", 0, { points: [[10, 10]] }))).toEqual({
      x: -4,
      y: -4,
      w: 28,
      h: 28,
    });
    // 两端点型缺第二点 → 无盒（不参与命中，而不是命中于一个零尺寸点）
    expect(boundsOf(ann("arrow", 0, { points: [[1, 1]] }))).toBeNull();
  });
});

describe("图层面板行模型与装配面", () => {
  it("overlayShot_layerPanelFourOpsPerRow", () => {
    const list = [ann("rect", 0), toggleLock([ann("ellipse", 1)], 0)[0], ann("text", 2)];
    const rows = layerRows(list);
    // 首行 = 最上层；index 回指入参数组下标（四钮的操作参数就是它）
    expect(rows.map((r) => [r.index, r.layer])).toEqual([
      [2, 2],
      [1, 1],
      [0, 0],
    ]);
    // 每行恒四钮，锁定行不减钮（"无钮的锁定行"就是本断言要拦的对象）
    for (const row of rows) expect(row.ops).toEqual(LAYER_OPS);
    expect(rows.filter((r) => r.locked)).toHaveLength(1);
    expect(rows.map((r) => r.name)).toEqual(["文字", "椭圆", "矩形"]);
    // 装配面判据：面板逐行按 row.ops 全量铺钮（OverlayShot 不含 canvas 之外的图层算法）
    expect(overlaySrc).toContain("row.ops.map(");
    expect(overlaySrc).toContain("LAYER_OP_LABEL[op]");
    expect(overlaySrc).toContain("applyLayerOp(row.index, op)");
  });
});
