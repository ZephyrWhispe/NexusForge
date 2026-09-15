import {
  createDarkTheme,
  createLightTheme,
  type BrandVariants,
  type Theme,
} from "@fluentui/react-components";

/**
 * NexusForge 主题（docs/UI-PLAN.md U1-2）
 *
 * 令牌来源：demo/index.html 的 CSS 变量（UI-DEMO.md §4 映射表）。
 * 规则：品牌槽位用 Fluent 标准 Windows 蓝渐变；中性色/状态色/圆角/字体
 * 按 demo 令牌覆写；业务组件禁止绕过 theme 引用硬编码颜色。
 */

/** Windows 蓝品牌渐变（Fluent 2 标准色阶，demo 强调色 #0078d4/#4cc2ff 落在 110/70 槽位） */
export const BRAND_RAMP: BrandVariants = {
  10: "#001324",
  20: "#001d3d",
  30: "#00284f",
  40: "#003262",
  50: "#003d76",
  60: "#00478a",
  70: "#00539e",
  80: "#005eb8",
  90: "#0068c4",
  100: "#0072d2",
  110: "#0078d4",
  120: "#2e8de8",
  130: "#57a3ef",
  140: "#83baf5",
  150: "#aed2fa",
  160: "#d7eafb",
};

const FONT_BASE =
  '"Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif';
const FONT_MONO = '"Cascadia Code", Consolas, "Cascadia Mono", monospace';

function applyOverrides(base: Theme, o: Partial<Theme>): Theme {
  return { ...base, ...o };
}

/* ---------------- 暗色主题（demo :root[data-theme="dark"]） ---------------- */

const darkOverrides: Partial<Theme> = {
  // 字体
  fontFamilyBase: FONT_BASE,
  fontFamilyMonospace: FONT_MONO,
  // 圆角体系（demo 6/8/12px）
  borderRadiusSmall: "4px",
  borderRadiusMedium: "6px",
  borderRadiusLarge: "8px",
  borderRadiusXLarge: "12px",
  // 三层背景（demo --bg / --bg-2 / --bg-3，Mica 基底）
  colorNeutralBackground1: "#1c1f26",
  colorNeutralBackground2: "#21252e",
  colorNeutralBackground3: "#272b36",
  colorNeutralBackground4: "#2d323e",
  colorNeutralBackground6: "#1c1f26",
  // 文字三级（demo --text / --text-2 / --text-3）
  colorNeutralForeground1: "#f2f3f6",
  colorNeutralForeground2: "#9ba1ac",
  colorNeutralForeground3: "#6e737d",
  colorNeutralForeground4: "#565b64",
  // 描边（demo --border / --border-strong，带透明度）
  colorNeutralStroke1: "#33384490",
  colorNeutralStroke2: "#2b303b",
  colorNeutralStrokeAccessible: "#4a505d",
  // 状态色（demo --ok / --warn / --err）
  colorPaletteGreenForeground1: "#6ccb5f",
  colorPaletteDarkOrangeForeground1: "#fcc02d",
  colorPaletteRedForeground1: "#ff99a4",
  colorPaletteYellowForeground1: "#fcc02d",
};

/* ---------------- 亮色主题（demo :root[data-theme="light"]） ---------------- */

const lightOverrides: Partial<Theme> = {
  fontFamilyBase: FONT_BASE,
  fontFamilyMonospace: FONT_MONO,
  borderRadiusSmall: "4px",
  borderRadiusMedium: "6px",
  borderRadiusLarge: "8px",
  borderRadiusXLarge: "12px",
  colorNeutralBackground1: "#eef2f7",
  colorNeutralBackground2: "#f6f8fb",
  colorNeutralBackground3: "#ffffff",
  colorNeutralBackground4: "#ffffff",
  colorNeutralForeground1: "#1a1d23",
  colorNeutralForeground2: "#5c6069",
  colorNeutralForeground3: "#8a8f99",
  colorNeutralForeground4: "#a0a5ae",
  colorNeutralStroke1: "#c3c8d2",
  colorNeutralStroke2: "#dde2ea",
  colorNeutralStrokeAccessible: "#7a7f8a",
  colorPaletteGreenForeground1: "#0f7b0f",
  colorPaletteDarkOrangeForeground1: "#9d5d00",
  colorPaletteRedForeground1: "#c42b1c",
  colorPaletteYellowForeground1: "#9d5d00",
};

export const nexusDarkTheme = applyOverrides(createDarkTheme(BRAND_RAMP), darkOverrides);
export const nexusLightTheme = applyOverrides(createLightTheme(BRAND_RAMP), lightOverrides);

/* ---------------- U1-3：Windows 强调色 → 动态品牌色阶 ---------------- */

function hexToRgb(hex: string): [number, number, number] {
  const h = hex.replace("#", "");
  return [
    parseInt(h.slice(0, 2), 16),
    parseInt(h.slice(2, 4), 16),
    parseInt(h.slice(4, 6), 16),
  ];
}

/** 返回 [h(0-360), s(0-1), l(0-1)] */
function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rn = r / 255, gn = g / 255, bn = b / 255;
  const max = Math.max(rn, gn, bn), min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === rn) h = ((gn - bn) / d + (gn < bn ? 6 : 0)) * 60;
  else if (max === gn) h = ((bn - rn) / d + 2) * 60;
  else h = ((rn - gn) / d + 4) * 60;
  return [h, s, l];
}

function hslToHex(h: number, s: number, l: number): string {
  const f = (n: number) => {
    const k = (n + h / 30) % 12;
    const a = s * Math.min(l, 1 - l);
    const v = l - a * Math.max(-1, Math.min(k - 3, Math.min(9 - k, 1)));
    return Math.round(v * 255).toString(16).padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

/** Fluent 16 档感知亮度目标（源自官方 Windows 蓝色阶各档实测 L 值） */
const RAMP_LIGHTNESS = [7, 12, 15, 19, 23, 27, 31, 36, 38, 41, 42, 55, 64, 74, 83, 91];
const RAMP_KEYS = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160] as const;

/** 从系统强调色生成品牌色阶：保留色相与饱和度，按 Fluent 亮度分布重排 */
export function rampFromAccent(accent: string): BrandVariants {
  const [h, s] = rgbToHsl(...hexToRgb(accent));
  const ramp = {} as BrandVariants;
  RAMP_KEYS.forEach((key, i) => {
    ramp[key] = hslToHex(h, Math.min(1, s), RAMP_LIGHTNESS[i] / 100);
  });
  return ramp;
}

const themeCache = new Map<string, { dark: Theme; light: Theme }>();

/** 构建主题集合（按强调色缓存）；accent 为 null 时使用默认 Windows 蓝 */
export function buildThemeSet(accent?: string | null): { dark: Theme; light: Theme } {
  const key = accent ?? "default";
  const hit = themeCache.get(key);
  if (hit) return hit;
  const ramp = accent ? rampFromAccent(accent) : BRAND_RAMP;
  const set = {
    dark: applyOverrides(createDarkTheme(ramp), darkOverrides),
    light: applyOverrides(createLightTheme(ramp), lightOverrides),
  };
  themeCache.set(key, set);
  return set;
}

/** 强调色实值（供 StatusBar 模块健康点等极少数非 Fluent 场景引用） */
export const STATUS_ACCENT = {
  dark: { ok: "#6ccb5f", warn: "#fcc02d", err: "#ff99a4", accent: "#4cc2ff" },
  light: { ok: "#0f7b0f", warn: "#9d5d00", err: "#c42b1c", accent: "#0078d4" },
} as const;
