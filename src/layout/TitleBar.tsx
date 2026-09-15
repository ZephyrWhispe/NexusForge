import {
  makeStyles,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import {
  ArrowMinimizeRegular,
  MaximizeRegular,
  DismissRegular,
} from "@fluentui/react-icons";

/** 标题栏（docs/DESIGN.md §3 主窗口装饰；U1-4 将接入 Mica 材质） */
const useStyles = makeStyles({
  root: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
    padding: "0 14px",
    userSelect: "none",
    backgroundColor: "transparent",
  },
  logo: {
    width: "18px",
    height: "18px",
    borderRadius: tokens.borderRadiusSmall,
    display: "grid",
    placeItems: "center",
    fontSize: "11px",
    fontWeight: 700,
    color: tokens.colorNeutralForegroundOnBrand,
    background: `linear-gradient(135deg, ${tokens.colorBrandForeground1}, ${tokens.colorBrandForeground2})`,
  },
  name: { fontWeight: tokens.fontWeightSemibold, fontSize: tokens.fontSizeBase300 },
  sub: { color: tokens.colorNeutralForeground3, marginLeft: "8px", fontSize: tokens.fontSizeBase200 },
  controls: {
    marginLeft: "auto",
    display: "flex",
  },
  ctrlBtn: {
    width: "44px",
    height: "30px",
    display: "grid",
    placeItems: "center",
    borderRadius: tokens.borderRadiusMedium,
    color: tokens.colorNeutralForeground2,
    backgroundColor: "transparent",
    border: "none",
    cursor: "pointer",
    ":hover": { backgroundColor: tokens.colorNeutralBackground1Hover },
  },
});

function useWindowControls() {
  const inTauri = "__TAURI_INTERNALS__" in window;
  const minimize = () => inTauri && void import("@tauri-apps/api/window").then((m) => m.getCurrentWindow().minimize());
  const toggleMaximize = () => inTauri && void import("@tauri-apps/api/window").then((m) => m.getCurrentWindow().toggleMaximize());
  const close = () => inTauri && void import("@tauri-apps/api/window").then((m) => m.getCurrentWindow().close());
  return { minimize, toggleMaximize, close };
}

export default function TitleBar() {
  const styles = useStyles();
  const win = useWindowControls();
  return (
    // data-tauri-drag-region：无框窗口拖拽区（Tauri 2 约定属性）
    <div className={styles.root} data-tauri-drag-region>
      <div className={styles.logo}>N</div>
      <span className={styles.name}>NexusForge</span>
      <span className={styles.sub}>阶段一 · 核心基础</span>
      <div className={styles.controls}>
        <Tooltip content="最小化" relationship="label">
          <button className={styles.ctrlBtn} onClick={win.minimize} aria-label="最小化">
            <ArrowMinimizeRegular />
          </button>
        </Tooltip>
        <Tooltip content="最大化" relationship="label">
          <button className={styles.ctrlBtn} onClick={win.toggleMaximize} aria-label="最大化">
            <MaximizeRegular />
          </button>
        </Tooltip>
        <Tooltip content="关闭" relationship="label">
          <button className={styles.ctrlBtn} onClick={win.close} aria-label="关闭">
            <DismissRegular />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
