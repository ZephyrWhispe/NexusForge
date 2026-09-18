import { useState } from "react";
import { makeStyles, tokens } from "@fluentui/react-components";
import { hostModuleRestart, type ModuleState } from "../ipc/client";
import { useModuleStatus } from "../stores/modules";
import { notify, reportError, useNotifications } from "../stores/notifications";

/**
 * 状态栏（docs/DESIGN.md §3.5 + §8.2，审查 D-19 / D-14）：
 * 模块健康点来自 useModuleStatus（host.module_state 事件直推，无轮询）；
 * Error 态模块渲染为重启按钮（stop→init→start 由宿主执行）；
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

function dotClass(state: ModuleState): "dotRun" | "dotErr" | "dotStopped" | "dotUninit" {
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

/** 浏览器预览（无宿主事件）时的静态默认展示 */
const FALLBACK: Record<string, ModuleState> = {
  clipboard: "Running",
  screenshot: "Running",
  ocr: "Running",
  proxy: "Stopped",
};

export default function StatusBar() {
  const styles = useStyles();
  const states = useModuleStatus((s) => s.states);
  const [restarting, setRestarting] = useState<string | null>(null);
  const unseenErrors = useNotifications((s) => s.unseenErrors);
  const flushUnseenErrors = useNotifications((s) => s.flushUnseenErrors);

  const restart = (id: string) => {
    setRestarting(id);
    hostModuleRestart(id)
      .then(() => notify("success", `模块 ${id} 已重启`))
      .catch((e) => reportError(e, { context: `模块 ${id} 重启失败` }))
      .finally(() => setRestarting(null));
  };

  const list = Object.entries(states).length > 0 ? states : FALLBACK;
  return (
    <div className={styles.root}>
      {Object.entries(list).map(([id, state]) =>
        state === "Error" ? (
          <button
            className={`${styles.restart} ${restarting === id ? styles.restartBusy : ""}`}
            key={id}
            disabled={restarting !== null}
            title="该模块已停止，点击重启"
            onClick={() => restart(id)}
          >
            <span className={`${styles.dot} ${styles.dotErr}`} />
            {restarting === id ? `${id} 重启中…` : id}
          </button>
        ) : (
          <span className={styles.st} key={id} title={`${id} · ${state}`}>
            <span className={`${styles.dot} ${styles[dotClass(state)]}`} />
            {id}
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
