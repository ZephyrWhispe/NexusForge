//! 标注图层模型（D-29 B4 T-B4-1）：z 序 / 锁定 / 点选 / 位移全部住在这里，
//! OverlayShot 只做"把事件翻成对这些纯函数的调用"。
//!
//! 外提的理由（§9.1-④）：覆盖层是 canvas + Tauri 窗口 API 的合体，jsdom 里既没有
//! 真实画布上下文也无法装载窗口——算法留在组件里就等于"只能靠实启冒烟验收"的算法。
//!
//! 坐标契约：`points` 是画布像素（不是 0..1 归一化），与 `applyAnn` 的绘制单位同源。

import type { AnnotationDto } from "../../ipc/client";

/** 图层化后的标注：`layer`/`locked` 在 DTO 上可选（旧历史可读），在本模块内恒有值 */
export interface Ann extends AnnotationDto {
  layer: number;
  locked: boolean;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 类型中文名（工具条确认框与图层面板同源，两处各自维护必然漂） */
export const ANN_KIND_NAME: Record<AnnotationDto["kind"], string> = {
  pen: "画笔",
  rect: "矩形",
  ellipse: "椭圆",
  arrow: "箭头",
  text: "文字",
  mosaic: "马赛克",
  number: "序号",
};

/** text 字号式与 number 圆半径：与 OverlayShot.applyAnn 的绘制参数逐字同源 */
const TEXT_FONT_BASE = 6;
const TEXT_FONT_EXTRA = 8;
const NUMBER_RADIUS = 14;
/** 文字宽度估算系数（无 2d 上下文可测量；命中测试只要一个可用的近似盒） */
const TEXT_CHAR_WIDTH = 0.6;

export function normalizeLayers(list: Ann[]): Ann[] {
  return list.map((a, i) => ({
    ...a,
    layer: Number.isFinite(a.layer) ? a.layer : i,
    locked: a.locked === true,
  }));
}

export function nextLayer(list: Ann[]): number {
  return list.reduce((max, a) => Math.max(max, a.layer + 1), 0);
}

/** 绘制序下标：layer 升序，同层保持入参序（与 Rust `Annotation::sort_by_layer` 同契约） */
function orderedIndices(list: Ann[]): number[] {
  return list
    .map((_, i) => i)
    .sort((a, b) => list[a].layer - list[b].layer || a - b);
}

export function sortByLayer(list: Ann[]): Ann[] {
  return orderedIndices(list).map((i) => list[i]);
}

/**
 * 只在 z 序里交换：与相邻层互换位置，其余条目的数组下标与内容一概不动。
 * 交换后全表重编号为 0..n-1——z 序是稠密概念，留空洞只会让下一次比较靠猜。
 */
export function moveLayer(list: Ann[], index: number, dir: -1 | 1): Ann[] {
  const order = orderedIndices(list);
  const pos = order.indexOf(index);
  const target = pos + dir;
  if (pos < 0 || target < 0 || target >= order.length) return list.map((a) => ({ ...a }));
  const tmp = order[pos];
  order[pos] = order[target];
  order[target] = tmp;
  const layerOf = new Map(order.map((itemIdx, slot) => [itemIdx, slot]));
  return list.map((a, i) => ({ ...a, layer: layerOf.get(i) ?? i }));
}

export function toggleLock(list: Ann[], index: number): Ann[] {
  return list.map((a, i) => (i === index ? { ...a, locked: !a.locked } : a));
}

export function removeAt(list: Ann[], index: number): Ann[] {
  return list.filter((_, i) => i !== index);
}

/** 命中盒：text/number 按锚点盒、pen/mosaic 按路径盒、两端点型按端点盒；无有效点返回 null */
export function boundsOf(ann: Ann): Box | null {
  const pts = ann.points;
  if (pts.length === 0) return null;
  const boxOf = (a: [number, number][], b: [number, number][]): Box => {
    const xs = [...a, ...b].map((p) => p[0]);
    const ys = [...a, ...b].map((p) => p[1]);
    const x = Math.min(...xs);
    const y = Math.min(...ys);
    return { x, y, w: Math.max(...xs) - x, h: Math.max(...ys) - y };
  };
  switch (ann.kind) {
    case "text": {
      const size = ann.width * TEXT_FONT_BASE + TEXT_FONT_EXTRA;
      return {
        x: pts[0][0],
        y: pts[0][1],
        w: (ann.text?.length ?? 1) * size * TEXT_CHAR_WIDTH,
        h: size,
      };
    }
    case "number":
      return {
        x: pts[0][0] - NUMBER_RADIUS,
        y: pts[0][1] - NUMBER_RADIUS,
        w: NUMBER_RADIUS * 2,
        h: NUMBER_RADIUS * 2,
      };
    case "pen":
    case "mosaic":
      return boxOf(pts, []);
    default:
      // rect / ellipse / arrow（以及后续新增的两端点型）：由两端点张成
      if (pts.length < 2) return null;
      return boxOf([pts[0]], [pts[1]]);
  }
}

export function translate(ann: Ann, dx: number, dy: number): Ann {
  return {
    ...ann,
    points: ann.points.map(([x, y]): [number, number] => [x + dx, y + dy]),
  };
}

/**
 * 从最上层往下找首个命中，返回其在 `list` 中的下标。
 * `skipLocked` 为真时锁定项整体穿透（不是"不可见"——绘制仍照常，见 sortByLayer）。
 */
export function hitTest(
  list: Ann[],
  pt: [number, number],
  size: [number, number],
  opts: { skipLocked: boolean },
): number | null {
  const order = orderedIndices(list);
  // 容差随画布尺寸缩放：细线在大图上按像素点命中几乎不可能
  const tol = Math.max(4, Math.min(size[0], size[1]) * 0.01);
  for (let k = order.length - 1; k >= 0; k--) {
    const i = order[k];
    const ann = list[i];
    if (opts.skipLocked && ann.locked) continue;
    const b = boundsOf(ann);
    if (!b) continue;
    if (
      pt[0] >= b.x - tol &&
      pt[0] <= b.x + b.w + tol &&
      pt[1] >= b.y - tol &&
      pt[1] <= b.y + b.h + tol
    ) {
      return i;
    }
  }
  return null;
}

export type LayerOp = "up" | "down" | "lock" | "delete";

/** 每行四钮：锁定行不减钮（"锁定"就是这一行的操作，减了反而无处解锁） */
export const LAYER_OPS: readonly LayerOp[] = ["up", "down", "lock", "delete"] as const;

export const LAYER_OP_LABEL: Record<LayerOp, string> = {
  up: "上移",
  down: "下移",
  lock: "锁定",
  delete: "删除",
};

export interface LayerRow {
  /** 在 `list` 中的下标（所有图层操作以此为参数） */
  index: number;
  name: string;
  color: string;
  layer: number;
  locked: boolean;
  ops: readonly LayerOp[];
}

/** 面板行模型：最上层在首行（与"图层"面板的通行读法一致） */
export function layerRows(list: Ann[]): LayerRow[] {
  return orderedIndices(list)
    .reverse()
    .map((i) => ({
      index: i,
      name: ANN_KIND_NAME[list[i].kind] ?? list[i].kind,
      color: list[i].color,
      layer: list[i].layer,
      locked: list[i].locked,
      ops: LAYER_OPS,
    }));
}
