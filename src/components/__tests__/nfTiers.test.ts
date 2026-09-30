import { describe, expect, it } from "vitest";

import { FORM_CARD_W, HIT_MIN, NF_MISSING, ROW_H, SHELL, SPACING, TIER_W, nfNum } from "../nfTiers";

// D-42 尺寸真源回归（00-ui-layout-spec 2/3/9-3/10-2 节）。
// 承重断言是"四的倍数"这条平台纪律本身：Windows 100–400% 缩放下 4 是唯一全乘得整的基元，
// 任何新增尺寸若脱离该基元即在此判红——而不是靠人眼在面板里找歪线。

function px(value: string): number {
  const matched = /^(-?\d+)px$/.exec(value);
  if (!matched) throw new Error(`尺寸值须为整数 px 字面量，实得：${value}`);
  return Number(matched[1]);
}

describe("nfTiers（D-42 尺寸真源）", () => {
  it("nfTiers_everyDimensionIsMultipleOfFour：间距/行高/宽度档/骨架高全部 %4==0", () => {
    const governed = {
      ...SPACING,
      ...ROW_H,
      ...TIER_W,
      HIT_MIN,
      FORM_CARD_W,
      // SHELL 逐枚入表：壳层骨架高是走查判据（48/40/32），漂移即红
      ...SHELL,
    } as Record<string, string | undefined>;
    const checked = Object.entries(governed).filter(([, v]) => v !== undefined);
    expect(checked.length).toBeGreaterThanOrEqual(18);
    // 在册唯一例外：subnav 190 是规范 1 节定值（%4≠0），登记在 nfTiers 注释与 D-42 实施记录里；
    // 除它以外任何值脱离 4 的基元即为本断言的判红面。
    const offGrid = checked
      .filter(([k]) => k !== "subnav")
      .filter(([, v]) => px(v as string) % 4 !== 0)
      .map(([k, v]) => `${k}=${v}`);
    expect(offGrid).toEqual([]);
    expect(SHELL.subnav).toBe("190px");
  });

  it("nfTiers_spacingIsWhitelistOnly：间距只允许 4/8/12/16/24（规范 2 节白名单）", () => {
    expect(Object.values(SPACING).map(px).sort((a, b) => a - b)).toEqual([4, 8, 12, 16, 24]);
  });

  it("nfTiers_rowHeightsAreThreeTiersAndTiersAreThreeSteps：行高三档 44/48/64 与宽度档 120/240/360", () => {
    expect(Object.values(ROW_H).map(px)).toEqual([44, 48, 64]);
    expect([TIER_W.s, TIER_W.m, TIER_W.l].map((v) => px(v as string))).toEqual([120, 240, 360]);
    // XL＝随容器（规范 3 档表的 fill 档），真源以 undefined 表达而非另造字面量
    expect(TIER_W.xl).toBeUndefined();
  });

  it("nfNum_placeholderIsDoubleDashForMissingOnly：缺值走 `--`，零与空串不混（10-15 防抖宽）", () => {
    expect(nfNum(undefined)).toBe(NF_MISSING);
    expect(nfNum(null)).toBe(NF_MISSING);
    expect(nfNum("")).toBe(NF_MISSING);
    expect(nfNum(0)).toBe("0");
    expect(nfNum(1234)).toBe("1234");
    // 正对照：占位符本身是两枚半角连字符（面板侧靠它区分"无数据"与"值为空"）
    expect(NF_MISSING).toBe("--");
  });
});
