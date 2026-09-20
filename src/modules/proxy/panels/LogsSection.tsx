import { useMemo, useState } from "react";
import { makeStyles, tokens, Button } from "@fluentui/react-components";
import type { ProxyLogLineDto } from "../../../ipc/client";
import Section from "../../../components/Section";
import EmptyState from "../../../components/EmptyState";
import DeferredBadge from "../../../components/DeferredBadge";
import { LOG_LEVELS, logLevelOf, type LogLevel } from "../logLevel";

/**
 * 日志子面板（T-B2-3，09 §5.2 字面）：级别芯片按行内级别词判定（logLevel.ts），
 * 纯前端过滤零新 invoke；清空只清本地列表（环形缓冲仍在后端）；复制全部走
 * navigator.clipboard.writeText；导出文件 [收窄]→B7 挂 DeferredBadge。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  logBox: {
    maxHeight: "320px",
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px",
  },
  grow: { flex: 1, minWidth: "240px" },
});

export default function LogsSection({
  logs,
  logsLoaded,
  onRefresh,
  onClear,
}: {
  logs: ProxyLogLineDto[];
  logsLoaded: boolean;
  onRefresh: () => void;
  onClear: () => void;
}) {
  const styles = useStyles();
  const [level, setLevel] = useState<LogLevel | "all">("all");
  const [msg, setMsg] = useState("");

  const shown = useMemo(
    () => (level === "all" ? logs : logs.filter((l) => logLevelOf(l.text) === level)),
    [logs, level],
  );

  const copyAll = () => {
    const text = logs.map((l) => l.text).join("\n");
    if (!navigator.clipboard?.writeText) {
      setMsg("剪贴板不可用（需安全上下文）");
      return;
    }
    navigator.clipboard
      .writeText(text)
      .then(() => setMsg(`已复制 ${logs.length} 行`))
      .catch(() => setMsg("复制失败"));
  };

  return (
    <Section
      title="内核日志"
      actions={
        <>
          <Button size="small" onClick={onRefresh}>
            刷新
          </Button>
          <DeferredBadge label="导出文件" decisionRef="B7" />
        </>
      }
    >
      <div className={styles.row}>
        <Button
          size="small"
          appearance={level === "all" ? "primary" : "outline"}
          onClick={() => setLevel("all")}
        >
          全部（{logs.length}）
        </Button>
        {LOG_LEVELS.map((lv) => {
          const n = logs.filter((l) => logLevelOf(l.text) === lv).length;
          return (
            <Button
              key={lv}
              size="small"
              appearance={level === lv ? "primary" : "outline"}
              onClick={() => setLevel(lv)}
            >
              {lv}（{n}）
            </Button>
          );
        })}
        <span className={styles.grow} />
        <Button size="small" onClick={copyAll}>
          复制全部
        </Button>
        <Button size="small" onClick={onClear}>
          清空
        </Button>
        {msg && <span className={styles.muted}>{msg}</span>}
      </div>
      <div className={styles.logBox}>
        {shown.length === 0 ? (
          <EmptyState
            text={
              logs.length === 0
                ? "暂无日志（内核未启动或未产生输出）"
                : `无 ${level} 级日志（共 ${logs.length} 行）`
            }
            loading={!logsLoaded}
          />
        ) : (
          shown.map((l, i) => (
            <div key={`${l.ts_ms}-${i}`} className={styles.mono}>
              {l.text}
            </div>
          ))
        )}
      </div>
    </Section>
  );
}
