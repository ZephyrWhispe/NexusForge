import { useEffect, useState } from "react";
import { makeStyles, tokens } from "@fluentui/react-components";
import { hostModulesStatus, type ModuleStatusDto } from "../ipc/client";

/** 状态栏（docs/DESIGN.md §3.5）：真实模块健康点（IPC）+ 快捷键提示 */
const useStyles = makeStyles({
  root: {
    display: "flex",
    alignItems: "center",
    gap: "16px",
    padding: "0 14px",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
    userSelect: "none",
    backgroundColor: "transparent",
  },
  st: { display: "flex", alignItems: "center", gap: "6px" },
  dot: {
    width: "7px",
    height: "7px",
    borderRadius: "50%",
  },
  dotRun: { background: tokens.colorPaletteGreenForeground1 },
  dotErr: { background: tokens.colorPaletteRedForeground1 },
  dotStopped: { background: tokens.colorPaletteDarkOrangeForeground1 },
  dotUninit: { background: tokens.colorNeutralStroke1 },
  hot: { marginLeft: "auto", color: tokens.colorNeutralForeground2 },
});

function dotClass(state: ModuleStatusDto["state"]): "dotRun" | "dotErr" | "dotStopped" | "dotUninit" {
  switch (state) {
    case "Running":
      return "dotRun";
    case "Error":
      return "dotErr";
    case "Stopped":
      return "dotStopped";
    default:
      return "dotUninit";
  }
}

/** 浏览器预览（无 IPC）时的静态默认展示 */
const FALLBACK: ModuleStatusDto[] = [
  { id: "clipboard", name: "clipboard", version: "", priority: 0, state: "Running" },
  { id: "screenshot", name: "screenshot", version: "", priority: 0, state: "Running" },
  { id: "ocr", name: "ocr", version: "", priority: 0, state: "Running" },
  { id: "proxy", name: "proxy", version: "", priority: 0, state: "Stopped" },
];

export default function StatusBar() {
  const styles = useStyles();
  const [modules, setModules] = useState<ModuleStatusDto[] | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () =>
      hostModulesStatus().then((m) => {
        if (alive && m) setModules(m);
      });
    load();
    const timer = window.setInterval(load, 2000); // 状态轮询：事件直推在 U2-3 接入
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  const list = modules ?? FALLBACK;
  return (
    <div className={styles.root}>
      {list.map((m) => (
        <span className={styles.st} key={m.id} title={`${m.name} · ${m.state}`}>
          <span className={`${styles.dot} ${styles[dotClass(m.state)]}`} />
          {m.id}
        </span>
      ))}
      <span className={styles.st}>SQLite WAL · 就绪</span>
      <span className={styles.hot}>Enter 粘贴 · Del 删除 · Ctrl+P 置顶</span>
    </div>
  );
}
