import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";
import { parseAppError, syncRunsGet, type SyncRunDto } from "../../ipc/client";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 同步活动流水（09 §10.2 T-B5-3）：每轮同步——含失败的那轮——都在这里看得见。
 *
 * 数据源是 `sync_runs_get`（落盘表），不是 `sync.run` 事件：摘要曾经只活在
 * tracing 日志与一次性广播里，重启即焚 ⇒ 面板说不清"上次到底同步了没"。
 * 承重④的对称面：`role` 区分本机发起 / 对端发起，被动侧供数结果同表可查。
 * 红线是"失败不静默"：错误行照常渲染并标红，一个都不过滤掉。
 * `refreshKey`（T-B5-8）是根面板的事件节流入口：它变化只**触发重读**，本视图
 * 从不把事件负载当成一行——数据的真源始终是那张表。
 */
const useStyles = makeStyles({
  item: {
    padding: "8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: "flex",
    flexDirection: "column",
    gap: "4px",
  },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  grow: { flex: 1, minWidth: "0" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  errorText: {
    color: tokens.colorPaletteRedForeground1,
    fontSize: tokens.fontSizeBase200,
    wordBreak: "break-all",
  },
});

const LIMIT = 50;

function tsText(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : "未知时间";
}

/**
 * 对端标签：握手完成后的行是设备 id（截前 8 位可读），握手前的失败行只有
 * socket 地址——身份无从得知，就把地址如实贴出来，不编一个"未知设备"。
 */
export function peerLabel(peer: string): string {
  return peer.includes(":") ? `地址 ${peer}（未完成握手）` : peer.slice(0, 8);
}

export default function ActivitySection({ refreshKey = 0 }: { refreshKey?: number }) {
  const styles = useStyles();
  const [rows, setRows] = useState<SyncRunDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      setRows(await syncRunsGet(LIMIT));
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  // refreshKey 变化 = 重新读表：新会话自然是列表首行（表按 ts 倒序），
  // 但那是读回来的结果，不是把事件负载插进 state——后者一旦丢事件就永久失踪。
  useEffect(() => {
    void load();
  }, [load, refreshKey]);

  return (
    <Section
      title={`同步活动（${rows.length}）`}
      actions={
        <>
          <Badge appearance="outline">每轮会话一行 · 失败同样入账</Badge>
          <Button size="small" onClick={() => void load()}>
            刷新
          </Button>
        </>
      }
    >
      <InlineError text={error} />
      {rows.length === 0 ? (
        <EmptyState
          text="暂无同步流水——第一回「立即同步」之后（无论成败）这里就会留下痕迹。"
          loading={!loaded}
        />
      ) : (
        rows.map((run) => (
          <div key={run.id} className={styles.item}>
            <div className={styles.row}>
              <Badge appearance="tint" color={run.error ? "danger" : "success"}>
                {run.error ? "失败" : "成功"}
              </Badge>
              <Text weight="semibold" size={300}>
                {run.role === "initiator" ? "本机发起" : "对端发起"}
              </Text>
              <Text className={styles.mono}>{peerLabel(run.peer)}</Text>
              <div className={styles.grow} />
              <Text className={styles.muted}>
                {tsText(run.tsMs)} · 耗时 {run.durationMs} ms
              </Text>
            </div>
            <Text className={styles.muted}>
              推送 {run.pushed} · 拉取应用 {run.pulledApplied} · 丢弃 {run.pulledLost} · 冲突{" "}
              {run.conflicts}
            </Text>
            {run.error && <div className={styles.errorText}>{run.error}</div>}
          </div>
        ))
      )}
    </Section>
  );
}
