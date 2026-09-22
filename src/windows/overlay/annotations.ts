//! 标注图层模型（D-29 B4 T-B4-1）：z 序 / 锁定 / 点选 / 位移全部住在这里，
//! OverlayShot 只做"把事件翻成对这些纯函数的调用"。
//!
//! 外提的理由（§9.1-④）：覆盖层是 canvas + Tauri 窗口 API 的合体，jsdom 里既没有
//! 真实画布上下文也无法装载窗口——算法留在组件里就等于"只能靠实启冒烟验收"的算法。
//!
//! 坐标契约：`points` 是画布像素（不是 0..1 归一化），与 `applyAnn` 的绘制单位同源。

import type { AnnotationDto } from "../../ipc/client";

/** 图层化后的标注：`layer`/`locked`/`alpha`/`fill` 在 DTO 上可选（旧历史可读），在本模块内恒有值 */
export interface Ann extends AnnotationDto {
  layer: number;
  locked: boolean;
  alpha: number;
  fill: boolean;
}

export type AnnKind = AnnotationDto["kind"];

/**
 * 覆盖层工具 = 标注类型 + 不产生标注的伪工具。
 *
 * `select` 只把 mousedown 交给图层命中测试（T-B4-1）；伪工具不进 `TOOL_KINDS`，
 * 因此也不会出现在 `ANN_KIND_NAME` / `RENDERERS` 两张按标注类型穷举的表里。
 */
export type Tool = AnnKind | "select" | "picker";

/**
 * 取色伪工具名单列（红线：不进 `TOOL_KINDS`/`ANN_KIND_NAME`/`RENDERERS`——拾色不产生标注）。
 * 三个"都不含它"由 `overlayPicker_notAnAnnotationKind` 钉住，见 T-B4-3。
 */
export const PICKER_TOOL = "picker" as const;
export const PICKER_LABEL = "取色器";

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 工具条顺序 = 本表顺序（组件不再自写字面数组，避免"加了 kind 忘了加工具"） */
export const TOOL_KINDS: readonly AnnKind[] = [
  "pen",
  "rect",
  "ellipse",
  "line",
  "arrow",
  "highlight",
  "text",
  "mosaic",
  "number",
  "blur",
];

/** 类型中文名（工具条确认框与图层面板同源，两处各自维护必然漂） */
export const ANN_KIND_NAME: Record<AnnKind, string> = {
  pen: "画笔",
  rect: "矩形",
  ellipse: "椭圆",
  line: "直线",
  arrow: "箭头",
  highlight: "荧光高亮",
  text: "文字",
  mosaic: "马赛克",
  number: "序号",
  blur: "模糊",
};

/** 工具条窄位缩写（与 ANN_KIND_NAME 分表：一个是图标，一个是全称） */
export const TOOL_ABBR: Record<AnnKind, string> = {
  pen: "笔",
  rect: "框",
  ellipse: "圆",
  line: "线",
  arrow: "箭头",
  highlight: "亮",
  text: "文",
  mosaic: "马",
  number: "①",
  blur: "模",
};

/** 绘制族：`stroke`=描边路径、`fill`=形状（受填充开关左右）、`pixel`=读改写像素、`text`=文字 */
export type Renderer = "stroke" | "fill" | "pixel" | "text";

/** 穷举表：Record 的编译期穷举即判据——新增 kind 而漏登记渲染族直接编不过 */
export const RENDERERS: Record<AnnKind, Renderer> = {
  pen: "stroke",
  line: "stroke",
  arrow: "stroke",
  highlight: "stroke",
  rect: "fill",
  ellipse: "fill",
  number: "fill",
  mosaic: "pixel",
  blur: "pixel",
  text: "text",
};

/** 只有形状类工具消费"填充"开关（画笔/高亮/模糊上放一个失效开关是噪声） */
export const SHAPE_TOOLS: readonly AnnKind[] = ["rect", "ellipse"];

/** 工具（含伪工具 `select`/`picker`）是否可用填充开关：形状类以外恒 false */
export function honoursFill(tool: Tool): boolean {
  return (SHAPE_TOOLS as readonly string[]).includes(tool);
}

/** 荧光笔语义：透明度上限（再高的 alpha 也钳到这里的值——永不遮字） */
export const HIGHLIGHT_ALPHA_MAX = 0.35;
/** 荧光笔线宽放大倍数（与 pen 的另一点区别；两处常量合起来就是"高亮 ≠ 画笔"的全部内容） */
export const HIGHLIGHT_WIDTH_SCALE = 4;

/** 提交时定格的笔画透明度：高亮类钳位，其余原样（0..1） */
export function strokeAlphaOf(kind: AnnKind, alpha: number): number {
  const a = Number.isFinite(alpha) ? Math.min(1, Math.max(0, alpha)) : 1;
  return kind === "highlight" ? Math.min(a, HIGHLIGHT_ALPHA_MAX) : a;
}


/** text 字号式与 number 圆半径：与 OverlayShot.applyAnn 的绘制参数逐字同源 */
const TEXT_FONT_BASE = 6;
const TEXT_FONT_EXTRA = 8;
const NUMBER_RADIUS = 14;
/** 文字宽度估算系数（无 2d 上下文可测量；命中测试只要一个可用的近似盒） */
const TEXT_CHAR_WIDTH = 0.6;

/**
 * DTO → 模块内标注：四个可选键（layer/locked/alpha/fill）在此定格。
 *
 * 历史 JSON 缺键是常态而非异常（T-B4-1 之前的行没有 layer，T-B4-2 之前的行没有 alpha），
 * 所以"补默认值"必须只有一个入口——否则每个读取点都要各自 `?? 1`，漏一个就是 undefined
 * 一路传进 canvas。高亮的 alpha 钳位也在这里：手工改坏的 JSON 读回来同样遮不了字。
 */
export function normalizeLayers(list: AnnotationDto[]): Ann[] {
  return list.map((a, i) => ({
    ...a,
    layer: Number.isFinite(a.layer) ? (a.layer as number) : i,
    locked: a.locked === true,
    alpha: strokeAlphaOf(a.kind, Number.isFinite(a.alpha) ? (a.alpha as number) : 1),
    fill: a.fill === true,
  }));
}

/** 新笔提交定格：layer 恒在当前最上层之上（增量绘制与全量重放同序的前提），锁定恒关 */
export function stampNewAnn(list: Ann[], draft: AnnotationDto): Ann {
  const [ann] = normalizeLayers([draft]);
  return { ...ann, layer: nextLayer(list), locked: false };
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
    case "highlight":
      return boxOf(pts, []);
    default:
      // rect / ellipse / line / arrow / blur（以及后续新增的两端点型）：由两端点张成
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

/** 文字内联编辑的草稿：位置为 canvas 像素，value 为输入框实时内容 */
export interface TextDraft {
  x: number;
  y: number;
  value: string;
}

/**
 * 草稿落定：只决定"要不要产生一条标注"，不碰画布。
 *
 * 取消（Esc）与空值/纯空白同样返回 null —— 两者的共同语义是"零标注零撤销栈条目"。
 * 这条纪律以前由 `window.prompt` 的返回值碰巧保证，内联编辑后必须由函数自己保证。
 */
export function finishTextDraft(
  draft: TextDraft | null,
  action: "commit" | "cancel",
  style: { color: string; width: number },
): AnnotationDto | null {
  if (!draft || action === "cancel") return null;
  const text = draft.value.trim();
  if (!text) return null;
  return {
    kind: "text",
    color: style.color,
    width: style.width,
    points: [[draft.x, draft.y]],
    text,
  };
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
