import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  parseAppError,
  syncConflictRestore,
  syncConflictsGet,
  type SyncConflictDto,
} from "../../ipc/client";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import { confirmAction } from "../../stores/confirm";

/**
 * 冲突历史（09 §10.2 T-B5-2）：LWW 判负一侧的内容快照在本机可见、可重新生效。
 *
 * 数据源是 `sync_conflicts_get`（落盘表），`sync.conflict` 事件只作提示——
 * 事件即焚是承重⑥的根因，视图不能把一次性广播当事实。
 * 文案红线：只说"以本地副本重新生效并推送"，不说"撤销对端/强制回滚"：
 * 本机无法保证对端在此之后不再修改，声称能撤销就是假保证。
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
  snapshot: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    maxHeight: "72px",
    overflowY: "auto",
    padding: "4px 6px",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRadius: tokens.borderRadiusSmall,
  },
});

/** 快照预览：删除标记与内容文本两形（内容只读展示，编辑归笔记面板） */
export function snapshotPreview(value: SyncConflictDto["lostValue"]): string {
  if (value.deleted) return "（该快照是一次删除：恢复即本机删掉这篇笔记）";
  const content = value.content ?? "";
  const firstLine = content.split(/\r?\n/)[0] ?? "";
  return firstLine.length > 120 ? `${firstLine.slice(0, 120)}…` : firstLine;
}

function tsText(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : "未知时间";
}

export default function ConflictsSection() {
  const styles = useStyles();
  const [rows, setRows] = useState<SyncConflictDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busyId, setBusyId] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const load = useCallback(async () => {
    try {
      setRows(await syncConflictsGet(50, 0));
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const restore = async (row: SyncConflictDto) => {
    const ok = await confirmAction({
      title: "以本地副本重新生效并推送",
      impact: [
        `笔记：${row.entityId}`,
        "把该冲突快照写回本机数据集，并作为本机新变更入变更流",
      ],
      detail:
        "下一轮同步会把它推给对端。对端在此之后若又改过，仍会按时间戳重新判胜负——" +
        "本操作不撤销对端、也不保证对端停在那一版。",
      command: row.entityId,
      confirmLabel: "重新生效并推送",
      danger: true,
    });
    if (!ok) return;
    setBusyId(row.conflictId);
    setError("");
    setNotice("");
    try {
      const r = await syncConflictRestore(row.conflictId);
      setNotice(`已以本机副本重新生效：${r.entityId}（新变更 ${r.opId}）`);
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusyId("");
    }
  };

  return (
    <Section
      title={`冲突历史（${rows.length}）`}
      actions={
        <>
          <Badge appearance="outline">LWW 败方快照 · 本机留存</Badge>
          <Button size="small" onClick={() => void load()}>
            刷新
          </Button>
        </>
      }
    >
      <InlineError text={error} />
      {!error && <InlineError text={notice} tone="success" />}
      {rows.length === 0 ? (
        <EmptyState
          text="暂无冲突记录——两端改动同一篇笔记时，判负那一侧的内容会留在这里可查可恢复。"
          loading={!loaded}
        />
      ) : (
        rows.map((row) => (
          <div key={row.conflictId} className={styles.item}>
            <div className={styles.row}>
              <Text weight="semibold" size={300}>
                {row.entityId}
              </Text>
              <div className={styles.grow} />
              <Button
                size="small"
                appearance="primary"
                disabled={busyId !== ""}
                onClick={() => void restore(row)}
              >
                以本地副本重新生效并推送
              </Button>
            </div>
            <Text className={styles.muted}>
              未采纳：{row.lostDevice.slice(0, 8)}（{tsText(row.lostTs)}）· 本机采纳：
              {row.winnerDevice.slice(0, 8)}（{tsText(row.winnerTs)}）· 记录于{" "}
              {tsText(row.recordedMs)}
            </Text>
            <div className={styles.snapshot}>{snapshotPreview(row.lostValue)}</div>
          </div>
        ))
      )}
    </Section>
  );
}
