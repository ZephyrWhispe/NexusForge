// 任务书字面判据集：工具穷举 / 透明度与填充落进标注载荷 / text 内联编辑（D-29 B4 T-B4-2）。
// 与 overlayAnnotations.test.ts 同一纪律：OverlayShot 在 jsdom 里装载不了（edit 阶段要真
// canvas 2d 上下文与 Tauri 窗口 API），所以判定分两半——语义半走纯模块函数，装配半走 ?raw 源码扫描。

import { describe, expect, it } from "vitest";
import {
  ANN_KIND_NAME,
  HIGHLIGHT_ALPHA_MAX,
  RENDERERS,
  TOOL_ABBR,
  TOOL_KINDS,
  finishTextDraft,
  normalizeLayers,
  stampNewAnn,
  type AnnKind,
} from "../overlay/annotations";
import type { AnnotationDto } from "../../ipc/client";
import overlaySrc from "../OverlayShot.tsx?raw";
import pixelSrc from "../overlay/pixel.ts?raw";

const STYLE = { color: "#1677ff", width: 4 };

function draft(kind: AnnKind, opts: Partial<AnnotationDto> = {}): AnnotationDto {
  return {
    kind,
    color: STYLE.color,
    width: STYLE.width,
    points: [
      [0, 0],
      [10, 10],
    ],
    ...opts,
  };
}

describe("工具穷举表", () => {
  it("overlayTools_everyKindHasNameAndRenderer", () => {
    const names = Object.keys(ANN_KIND_NAME);
    const renderers = Object.keys(RENDERERS);
    const abbrs = Object.keys(TOOL_ABBR);
    // 三面键集必须与 TOOL_KINDS 完全相等（漏一项即红：任一面少一个键就有一支工具没有中文名或没有渲染族）
    expect([...TOOL_KINDS].sort()).toEqual([...names].sort());
    expect([...TOOL_KINDS].sort()).toEqual([...renderers].sort());
    expect([...TOOL_KINDS].sort()).toEqual([...abbrs].sort());
    const arms = new Set(["stroke", "fill", "pixel", "text"]);
    for (const kind of TOOL_KINDS) {
      expect(ANN_KIND_NAME[kind]).not.toBe(kind); // 中文名，不是枚举字面量兜过去的假阳性
      expect(ANN_KIND_NAME[kind].length).toBeGreaterThan(0);
      expect(TOOL_ABBR[kind].length).toBeGreaterThan(0);
      expect(arms.has(RENDERERS[kind])).toBe(true);
    }
    // T-B4-2 的三支新工具必须在表内且族别正确
    expect(RENDERERS.line).toBe("stroke");
    expect(RENDERERS.highlight).toBe("stroke");
    expect(RENDERERS.blur).toBe("pixel");
    // 装配面：工具条不再自写字面数组，而是遍历 TOOL_KINDS 并取 TOOL_ABBR
    expect(overlaySrc).toContain("TOOL_KINDS.map(");
    expect(overlaySrc).toContain("{TOOL_ABBR[t]}");
  });
});

describe("text 内联编辑", () => {
  it("overlayText_inlineEdit_commitOnEnter_cancelOnEsc_zeroAnnotation", () => {
    const d = { x: 12, y: 34, value: "  注意这里  " };
    const done = finishTextDraft(d, "commit", STYLE);
    expect(done).not.toBeNull();
    expect(done?.kind).toBe("text");
    expect(done?.points).toEqual([[12, 34]]);
    expect(done?.text).toBe("注意这里"); // 提交侧 trim，与"取消=空值"同一判据源
    // 红线取消臂：Esc 与空值/纯空白都不产生标注（因而零撤销栈条目）
    expect(finishTextDraft(d, "cancel", STYLE)).toBeNull();
    expect(finishTextDraft({ x: 1, y: 2, value: "" }, "commit", STYLE)).toBeNull();
    expect(finishTextDraft({ x: 1, y: 2, value: "   " }, "commit", STYLE)).toBeNull();
    // 已落定（凭据被取走）后再来一次仍是零标注：Enter 紧跟 blur 不会提交两遍
    expect(finishTextDraft(null, "commit", STYLE)).toBeNull();
    // 装配面：window.prompt 已字面出局，输入框自带 Enter/Esc 两个出口
    expect(overlaySrc).not.toContain("window.prompt");
    expect(overlaySrc).toContain('settleText("commit")');
    expect(overlaySrc).toContain('settleText("cancel")');
    expect(overlaySrc).toContain("data-text-draft");
  });
});

describe("透明度与填充", () => {
  it("overlayHighlight_neverFullyOpaque", () => {
    // 表观判据落在"提交出的 AnnotationDto.alpha"上：钳位发生在定格点，不是在绘制点顺眼一下
    const hi = stampNewAnn([], draft("highlight", { alpha: 1 }));
    expect(hi.alpha).toBeLessThanOrEqual(HIGHLIGHT_ALPHA_MAX);
    expect(hi.alpha).toBe(HIGHLIGHT_ALPHA_MAX);
    // 正对照：其余工具不遭钳位（下限方向的 alpha 也原样保留）
    expect(stampNewAnn([], draft("pen", { alpha: 1 })).alpha).toBe(1);
    expect(stampNewAnn([], draft("highlight", { alpha: 0.2 })).alpha).toBe(0.2);
    // 手工改坏的历史 JSON 读回来同样遮不了字
    expect(normalizeLayers([draft("highlight", { alpha: 0.99 })])[0].alpha).toBe(
      HIGHLIGHT_ALPHA_MAX,
    );
  });

  it("overlayAlphaAndFill_reachAnnotationPayload", () => {
    const ann = stampNewAnn([], draft("rect", { fill: true, alpha: 0.5 }));
    expect(ann).toEqual(expect.objectContaining({ fill: true, alpha: 0.5 }));
    // 同一定格点也兑现 T-B4-1 的 layer/locked 契约（新笔恒最上层）
    expect(ann.layer).toBe(0);
    expect(ann.locked).toBe(false);
    expect(stampNewAnn([ann], draft("ellipse")).layer).toBe(1);
    // 缺省两键：旧形状条目 = 描边 + 全不透明
    const legacy = stampNewAnn([], draft("ellipse"));
    expect(legacy.fill).toBe(false);
    expect(legacy.alpha).toBe(1);
    // 装配面：样式条三件套（自定义色 / 透明度 Slider / 填充 Switch）与笔画定格同源
    expect(overlaySrc).toContain('type="color"');
    expect(overlaySrc).toContain("<Slider");
    expect(overlaySrc).toContain("<Switch");
    expect(overlaySrc).toContain("alpha: strokeAlpha, fill: fillShape");
  });

  it("overlayBlur_usesPixelPassNotCanvasFilter", () => {
    expect(RENDERERS.blur).toBe("pixel");
    // 静态红线：区域模糊走像素遍历，不借 canvas 滤镜（jsdom 无画布后端 + 重放序不可控）
    expect(overlaySrc).not.toContain("ctx.filter");
    expect(overlaySrc).not.toContain("filter =");
    expect(overlaySrc).toContain("applyBoxBlurPass(");
    expect(pixelSrc).not.toContain("filter");
  });
});
