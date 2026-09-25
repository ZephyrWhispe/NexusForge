/**
 * STD-05（D-37 R-I3）色值收口：业务代码中"必须具体色值"的唯一落点。
 *
 * 只收两类不走 Fluent token 通道的颜色——
 * ① 非 DOM 着色面（canvas 2D 绘制、xterm 底色、JPEG 无 alpha 合成底）：
 *    canvas/xterm 不消费 CSS 变量，token 化为伪需求；
 * ② 覆盖层独立窗口 chrome（截屏覆盖层 / 贴图窗 / 滚动步进条）：深色 scrim
 *    刻意与主窗口明暗主题解耦（选区/编辑期恒暗），其上白字必须随 scrim
 *    而非随主题，token 化反而制造主题切换下的不可读。
 * 各消费点测试里的字面量是本文件数值的正对照钉（值面锁定），刻意保留。
 */

/** 截屏覆盖层 / 贴图窗 chrome（04 §截图；恒暗系） */
export const OVERLAY_CHROME = {
  /** 整窗垫黑：截图帧铺进 backdrop 前的瞬时底 */
  matte: "#000",
  /** 选区外调暗遮罩 */
  dim: "rgba(0,0,0,0.45)",
  /** 选区描边（半透明白）与其外侧一圈暗封边 */
  selectionBorder: "#ffffffcc",
  selectionEdge: "rgba(0,0,0,0.6)",
  /** 选区外"抠亮"用的超大 outline 色 */
  selectionMask: "rgba(0,0,0,0.55)",
  /** 浮动 chip（提示条/尺寸徽标）底色与其上的文字色 */
  chipBg: "rgba(28,28,30,0.92)",
  onDark: "#fff",
  /** 编辑阶段整窗背景 */
  editBg: "rgba(20,20,22,0.92)",
  /** 画布卡片投影 */
  canvasShadow: "0 8px 30px rgba(0,0,0,0.6)",
  /** 滚动截图步进条底色 */
  stripBg: "rgba(0,0,0,.72)",
  /** 贴图窗角标（比主 chip 更透一档） */
  pinChipBg: "rgba(28,28,30,0.85)",
} as const;

/** 标注画布（canvas ctx 消费；随导出图像烧死，不随主题）。刻意不 `as const`：色值即 string，供 state 自由流转 */
export const ANNOTATION = {
  /** 工具栏色板（顺序即 UI 顺序，末位为白） */
  swatches: ["#ff4d4f", "#ffb020", "#52c41a", "#1677ff", "#ffffff"],
  /** 默认笔画色与选中框虚线色（与色板第 4 档同值） */
  accent: "#1677ff",
  /** 序号圆标内文字 */
  seqFg: "#ffffff",
};

/** Mica 失效时的渐变兜底（15 §Mica；RGB 对应 bg1/bg2/bg3 token 值 + 边缘透明度） */
export const MICA_FALLBACK = {
  dark: "linear-gradient(135deg, rgba(28,31,38,0.96) 0%, rgba(32,36,46,0.94) 55%, rgba(27,32,40,0.96) 100%)",
  light:
    "linear-gradient(135deg, rgba(238,242,247,0.96) 0%, rgba(232,237,245,0.94) 60%, rgba(227,235,246,0.96) 100%)",
} as const;

/** xterm 终端容器底色：恒暗（xterm canvas 不消费 CSS 变量） */
export const TERMINAL_BG = "#1b1b1b";

/** 与 Rust `JPEG_BG_RGB` 同语义：JPEG 无 alpha，显式压白底而不是让浏览器丢通道 */
export const JPEG_MATTE = "#ffffff";

/** 美化预设渐变（bgFrom/bgTo；烧进导出图，不随主题） */
export const BEAUTIFY_BG = {
  none: { from: "#000000", to: "#000000" },
  card: { from: "#1f2937", to: "#0b1220" },
  social: { from: "#7c3aed", to: "#0ea5e9" },
} as const;
