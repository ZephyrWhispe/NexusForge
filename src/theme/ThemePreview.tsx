import {
  FluentProvider,
  makeStyles,
  tokens,
  type Theme,
} from "@fluentui/react-components";
import { BRAND_RAMP, nexusDarkTheme, nexusLightTheme } from "./theme";

/**
 * 主题基线页（docs/UI-PLAN.md U1-2 验收物，替代暂缓的 Storybook 基线页）：
 * 通过 ?w=theme-preview 打开，左右分栏对照亮/暗两套主题，
 * 展示品牌色阶、三层背景、三级文字、状态色、圆角与字体。
 */
const useStyles = makeStyles({
  root: {
    display: "grid",
    gridTemplateColumns: "1fr 1fr",
    height: "100vh",
  },
  pane: {
    padding: "24px",
    overflowY: "auto",
  },
  title: {
    fontSize: tokens.fontSizeBase500,
    fontWeight: tokens.fontWeightSemibold,
    marginBottom: "12px",
    display: "block",
  },
  section: { marginBottom: "18px" },
  label: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    marginBottom: "4px",
    display: "block",
  },
  ramp: {
    display: "flex",
    borderRadius: tokens.borderRadiusMedium,
    overflow: "hidden",
  },
  chip: { width: "36px", height: "36px" },
  rows: { display: "grid", gap: "4px" },
  row: {
    padding: "8px 12px",
    borderRadius: tokens.borderRadiusMedium,
    background: tokens.colorNeutralBackground3,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  bgs: { display: "flex", gap: "8px" },
  bg: {
    flex: "1",
    height: "48px",
    borderRadius: tokens.borderRadiusLarge,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  status: { display: "flex", gap: "16px" },
  dot: { display: "inline-block", width: "10px", height: "10px", borderRadius: "50%", marginRight: "6px" },
});

const BRAND_STEPS = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160] as const;

function Pane({ theme, name }: { theme: Theme; name: string }) {
  const styles = useStyles();
  return (
    <FluentProvider theme={theme}>
      <div className={styles.pane}>
        <span className={styles.title}>{name}</span>

        <div className={styles.section}>
          <span className={styles.label}>品牌色阶（Windows 蓝 10–160）</span>
          <div className={styles.ramp}>
            {BRAND_STEPS.map((n) => (
              <div
                key={n}
                className={styles.chip}
                title={`brand ${n}`}
                style={{ background: BRAND_RAMP[n] }}
              />
            ))}
          </div>
        </div>

        <div className={styles.section}>
          <span className={styles.label}>三层背景（Mica 基底 → 面板）</span>
          <div className={styles.bgs}>
            <div className={styles.bg} style={{ background: tokens.colorNeutralBackground1 }} />
            <div className={styles.bg} style={{ background: tokens.colorNeutralBackground2 }} />
            <div className={styles.bg} style={{ background: tokens.colorNeutralBackground3 }} />
          </div>
        </div>

        <div className={styles.section}>
          <span className={styles.label}>三级文字</span>
          <div className={styles.rows}>
            <div className={styles.row} style={{ color: tokens.colorNeutralForeground1 }}>
              前景 1 —— 主文本：剪切板历史、设置标题
            </div>
            <div className={styles.row} style={{ color: tokens.colorNeutralForeground2 }}>
              前景 2 —— 次文本：来源应用、时间戳
            </div>
            <div className={styles.row} style={{ color: tokens.colorNeutralForeground3 }}>
              前景 3 —— 辅助文本：快捷键提示、空状态
            </div>
          </div>
        </div>

        <div className={styles.section}>
          <span className={styles.label}>状态色（ok / warn / err）</span>
          <div className={styles.status}>
            <span>
              <span className={styles.dot} style={{ background: tokens.colorPaletteGreenForeground1 }} />
              Running
            </span>
            <span>
              <span className={styles.dot} style={{ background: tokens.colorPaletteDarkOrangeForeground1 }} />
              未启用
            </span>
            <span>
              <span className={styles.dot} style={{ background: tokens.colorPaletteRedForeground1 }} />
              Error
            </span>
          </div>
        </div>

        <div className={styles.section}>
          <span className={styles.label}>圆角 4 / 6 / 8 / 12px 与字体</span>
          <div className={styles.rows}>
            <div className={styles.row} style={{ borderRadius: tokens.borderRadiusSmall }}>
              Small 4px — chip
            </div>
            <div className={styles.row} style={{ borderRadius: tokens.borderRadiusMedium }}>
              Medium 6px — 按钮 / 输入框
            </div>
            <div className={styles.row} style={{ borderRadius: tokens.borderRadiusLarge }}>
              Large 8px — 条目卡片
            </div>
            <div className={styles.row} style={{ borderRadius: tokens.borderRadiusXLarge }}>
              XLarge 12px — 快速面板（等宽字体样例：let x = tokio::spawn(f());）
            </div>
          </div>
        </div>
      </div>
    </FluentProvider>
  );
}

export default function ThemePreview() {
  return (
    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", height: "100vh" }}>
      <Pane theme={nexusLightTheme} name="NexusForge Light" />
      <Pane theme={nexusDarkTheme} name="NexusForge Dark" />
    </div>
  );
}
