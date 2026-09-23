import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
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
import DeferredBadge from "../../components/DeferredBadge";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import { confirmAction } from "../../stores/confirm";

/**
 * 冲突历史（09 §10.2 T-B5-2 + T-B5-8 视图面）：败方快照要看得见，恢复要问一声。
 *
 * - 数据源是 `sync_conflicts_get`（落盘表），`sync.conflict` 事件只作提示——
 *   事件即焚是承重⑥的根因，视图不能把一次性广播当事实。根收到 `refreshKey` 变化时
 *   **重新去读表**（不是把事件里的东西插进列表），所以"错过事件"不会永久失踪。
 * - 快照查看 Dialog 是**只读**的（14-sync §5-4 红线）：渲染树里没有导出、没有复制按钮。
 *   败方内容是别台设备写过、又被 LWW 判负的字句，把它做成一键外流的口子等于让
 *   一条"已被丢弃"的记录新开一个泄漏面；要拿内容就去「以本地副本重新生效」，
 *   那条路有确认框、会入变更流、留下账。
 * - 文案红线：只说"以本地副本重新生效并推送"，不说"撤销对端/强制回滚"：
 *   本机无法保证对端在此之后不再修改，声称能撤销就是假保证。
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
  /** 只读快照全文：比行内预览高，滚动看全文——但没有一键外流的出口 */
  snapshotFull: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    maxHeight: "320px",
    overflowY: "auto",
    padding: "6px 8px",
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

/** 快照全文（只读 Dialog 用）：删除标记与内容两种形，内容原样吐出不加工 */
export function snapshotFull(value: SyncConflictDto["lostValue"]): string {
  if (value.deleted) return "（该快照是一次删除：恢复即本机删掉这篇笔记）";
  const title = value.title ? `${value.title}\n\n` : "";
  return `${title}${value.content ?? "（快照无正文字段——恢复仍按落盘的整份值写回）"}`;
}

function tsText(ms: number): string {
  return ms > 0 ? new Date(ms).toLocaleString() : "未知时间";
}

export default function ConflictsSection({
  refreshKey = 0,
  initialViewer = null,
}: {
  refreshKey?: number;
  /** 只读快照查看的初始打开行（测试用注入点；UI 侧一律从 null 起步） */
  initialViewer?: SyncConflictDto | null;
}) {
  const styles = useStyles();
  const [rows, setRows] = useState<SyncConflictDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [busyId, setBusyId] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  /** 只读快照查看（null=关闭；存整行，Dialog 不再回查表） */
  const [viewer, setViewer] = useState<SyncConflictDto | null>(initialViewer);

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

  // refreshKey 变化 = 重新读表（事件只作提示：把广播里的东西插进列表就是"错过即永久失踪"的老路）
  useEffect(() => {
    void load();
  }, [load, refreshKey]);

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
          <DeferredBadge label="三方合并" decisionRef="09 §10.2-9(a)" />
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
              <Button size="small" appearance="subtle" onClick={() => setViewer(row)}>
                查看快照
              </Button>
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

      {/* 只读快照查看（14-sync §5-4 红线）：DialogActions 里只有「关闭」一枚按钮——
          复制/导出按钮不存在于渲染树，Test 侧按"零外流入口"扫断言，这里不解释得比断言更宽。 */}
      <Dialog open={viewer !== null} onOpenChange={() => setViewer(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>冲突快照（只读）</DialogTitle>
            <DialogContent>
              {viewer && (
                <>
                  <Text className={styles.muted}>
                    {viewer.entityId} · 未采纳自 {viewer.lostDevice.slice(0, 8)}（
                    {tsText(viewer.lostTs)}）
                  </Text>
                  <div className={styles.snapshotFull}>{snapshotFull(viewer.lostValue)}</div>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="primary" onClick={() => setViewer(null)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </Section>
  );
}
