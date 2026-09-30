/**
 * 布局尺寸唯一事实源（docs/panels/2026-09-19/00-ui-layout-spec.md）。
 * 面板侧 makeStyles 一律展开本表，禁各写各的字面量——D-42 的布局合规门禁据此反查内联宽度。
 * 取值纪律：规范 2 节间距白名单 4/8/12/16/24、3 节控件宽度档、9-3 行高三档、10-2 四的倍数。
 */

export type ControlTier = "s" | "m" | "l" | "xl";

/** S=数字/端口 M=名称 L=路径/URL XL=fill（undefined 表示随容器，由调用方给 maxWidth） */
export const TIER_W: Record<ControlTier, string | undefined> = {
  s: "120px",
  m: "240px",
  l: "360px",
  xl: undefined,
};

export const ROW_H = { compact: "44px", base: "48px", twoline: "64px" } as const;

export const SPACING = { x4: "4px", x8: "8px", x12: "12px", x16: "16px", x24: "24px" } as const;

/** 壳层骨架高度与留白；subnav 190 是规范 1 节定值（%4≠0 的在册例外，改它须先改规范） */
export const SHELL = {
  header: "48px",
  toolbar: "40px",
  footer: "32px",
  subnav: "190px",
  subnavCompact: "48px",
  contentMax: "1040px",
  contentPad: "20px",
} as const;

/** 交互命中区下限（规范 10-16：紧凑态仍 ≥40×40 逻辑 px） */
export const HIT_MIN = "40px";

/** 单列表单卡宽度档（建库/解锁这类"一次只填一件事"的表单容器；420＝现值且 %4==0） */
export const FORM_CARD_W = "420px";

/** 数字/日期列缺值占位（规范 2 节：延迟 `--` 占位防抖宽） */
export const NF_MISSING = "--";

export function nfNum(value: number | string | null | undefined): string {
  return value === null || value === undefined || value === "" ? NF_MISSING : String(value);
}

/** 单元格槽位：makeStyles 内展开即用，行缝补数据的面板共用（规范 2 节表列纪律＋10-15 防抖宽） */
export const nfSlots = {
  textCell: {
    minWidth: "120px",
    overflowX: "hidden" as const,
    textOverflow: "ellipsis" as const,
    whiteSpace: "nowrap" as const,
  },
  numCell: {
    fontVariantNumeric: "tabular-nums",
    textAlign: "end" as const,
    minWidth: "64px",
    paddingLeft: SPACING.x12,
  },
  dateCell: {
    fontVariantNumeric: "tabular-nums",
    textAlign: "end" as const,
    minWidth: "96px",
    paddingLeft: SPACING.x12,
  },
  iconBtn: {
    minWidth: "24px",
    minHeight: "24px",
    padding: SPACING.x4,
  },
} as const;
