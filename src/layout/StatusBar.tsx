import { useEffect, useState } from "react";
import { makeStyles, tokens } from "@fluentui/react-components";
import { hostModuleRestart, hostModulesStatus, type ModuleStatusDto } from "../ipc/client";
import { notify, reportError, useNotifications } from "../stores/notifications";

/**
 * 状态栏（docs/DESIGN.md §3.5 + §8.2，审查 D-19）：
 * 真实模块健康点（IPC）；Error 态模块渲染为重启按钮（stop→init→start 由宿主执行）；
 * 全局通道有未读错误时显示红点角标，点击回放最近错误。
 */
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
  restart: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    border: "none",
    background: "transparent",
    padding: 0,
    font: "inherit",
    color: tokens.colorPaletteRedForeground1,
    cursor: "pointer",
  },
  restartBusy: { cursor: "wait", opacity: 0.7 },
  badge: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    border: "none",
    background: "transparent",
    padding: 0,
    font: "inherit",
    color: tokens.colorNeutralForeground3,
    cursor: "pointer",
  },
  badgeDot: {
    width: "7px",
    height: "7px",
    borderRadius: "50%",
    background: tokens.colorPaletteRedBackground3,
  },
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
  const [restarting, setRestarting] = useState<string | null>(null);
  const unseenErrors = useNotifications((s) => s.unseenErrors);
  const flushUnseenErrors = useNotifications((s) => s.flushUnseenErrors);

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

  const restart = (id: string) => {
    setRestarting(id);
    hostModuleRestart(id)
      .then(() => notify("success", `模块 ${id} 已重启`))
      .catch((e) => reportError(e, { context: `模块 ${id} 重启失败` }))
      .finally(() => setRestarting(null));
  };

  const list = modules ?? FALLBACK;
  return (
    <div className={styles.root}>
      {list.map((m) =>
        m.state === "Error" ? (
          <button
            className={`${styles.restart} ${restarting === m.id ? styles.restartBusy : ""}`}
            key={m.id}
            disabled={restarting !== null}
            title="该模块已停止，点击重启"
            onClick={() => restart(m.id)}
          >
            <span className={`${styles.dot} ${styles.dotErr}`} />
            {restarting === m.id ? `${m.id} 重启中…` : m.id}
          </button>
        ) : (
          <span className={styles.st} key={m.id} title={`${m.name} · ${m.state}`}>
            <span className={`${styles.dot} ${styles[dotClass(m.state)]}`} />
            {m.id}
          </span>
        ),
      )}
      {unseenErrors > 0 && (
        <button className={styles.badge} title="有错误发生，点击查看" onClick={flushUnseenErrors}>
          <span className={styles.badgeDot} />
          {unseenErrors} 条错误
        </button>
      )}
      <span className={styles.st}>SQLite WAL · 就绪</span>
      <span className={styles.hot}>Enter 粘贴 · Del 删除 · Ctrl+P 置顶</span>
    </div>
  );
}
