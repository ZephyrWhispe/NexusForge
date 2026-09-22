import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  Input,
  makeStyles,
  Text,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import {
  clipboardGroupCounts,
  clipboardGroupDelete,
  clipboardGroupRename,
  clipboardSuggestions,
  clipboardSuggestionApply,
  parseAppError,
  type ClipSuggestion,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify, reportError } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";
import EmptyState from "../../../components/EmptyState";
import { GROUP_LABEL } from "../display";

/**
 * 收藏与分组子面板（T-B3-4，细案 01§2/§5-1 + §7.1）：左分组树 + 右「建议归组」卡墙。
 * 建议制红线：分类器只出建议，落库分组必须由用户点「采纳」——卡墙上没有自动勾选，
 * 「忽略」也绝不改分组（只把这条建议永久静默）。
 */
const useStyles = makeStyles({
  root: { display: "flex", flex: 1, minHeight: 0, gap: "16px", padding: "14px 20px 20px" },
  col: {
    flex: 1,
    minWidth: 0,
    display: "flex",
    flexDirection: "column",
    overflowY: "auto",
  },
  head: {
    display: "flex",
    alignItems: "baseline",
    gap: "8px",
    marginBottom: "4px",
  },
  title: { fontSize: tokens.fontSizeBase400, fontWeight: tokens.fontWeightSemibold },
  hint: {
    display: "block",
    marginBottom: "10px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  row: {
    display: "flex",
    alignItems: "center",
    gap: "10px",
    padding: "9px 12px",
    borderRadius: tokens.borderRadiusMedium,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  name: { flex: 1, minWidth: 0, fontSize: tokens.fontSizeBase300 },
  builtin: { color: tokens.colorNeutralForeground3 },
  card: {
    padding: "10px 12px",
    marginBottom: "8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  preview: {
    display: "block",
    fontSize: tokens.fontSizeBase300,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  meta: {
    display: "block",
    margin: "4px 0 8px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
  },
  ops: { display: "flex", gap: "6px" },
});

/** text/files/secret/all 是 SubNav 的筛选桶而非用户分组：没有实体可改名或删除 */
function isBuiltinBucket(group: string): boolean {
  return group === "all" || group === "text" || group === "files" || group === "secret";
}

export default function GroupsSection() {
  const styles = useStyles();
  const [counts, setCounts] = useState<Record<string, number>>({});
  const [sugg, setSugg] = useState<ClipSuggestion[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    if (!IN_TAURI) {
      setLoaded(true);
      return;
    }
    clipboardGroupCounts()
      .then((c) => setCounts(c as Record<string, number>))
      .catch((e) => reportError(e, { context: "分组计数读取失败", dedupeKey: "clip-groups", toast: false }))
      .finally(() => setLoaded(true));
    clipboardSuggestions(100)
      .then(setSugg)
      .catch((e) => reportError(e, { context: "分组建议读取失败", dedupeKey: "clip-suggestions", toast: false }));
  }, []);

  useEffect(refresh, [refresh]);

  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic;
          if (topic === "clipboard.groups_changed" || topic === "clipboard.captured") refresh();
        }),
      )
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  const startRename = (from: string) => {
    setEditing(from);
    setDraft(from);
  };

  const commitRename = async () => {
    const from = editing;
    const to = draft.trim();
    setEditing(null);
    if (from === null || to === "" || to === from) return;
    setBusy(true);
    try {
      const n = await clipboardGroupRename(from, to);
      notify("success", "分组已重命名", `「${to}」接管 ${n} 条条目`);
      refresh();
    } catch (e) {
      notify("error", "重命名失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  const doDeleteGroup = async (name: string) => {
    if (
      !(await confirmAction({
        title: `删除分组「${name}」`,
        impact: [`${counts[name] ?? 0} 条条目回到未分组`],
        detail: "只解除分组归属，历史记录本身保留。",
        confirmLabel: "删除分组",
      }))
    )
      return;
    setBusy(true);
    try {
      const n = await clipboardGroupDelete(name);
      notify("success", "分组已删除", `${n} 条条目改为未分组`);
      refresh();
    } catch (e) {
      notify("error", "删除分组失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  const doApply = async (s: ClipSuggestion, accept: boolean) => {
    setBusy(true);
    try {
      await clipboardSuggestionApply([s.entry_id], accept);
      setSugg((cur) => cur.filter((x) => x.entry_id !== s.entry_id));
      refresh();
    } catch (e) {
      notify("error", accept ? "采纳建议失败" : "忽略建议失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  const groups = Object.entries(counts)
    .filter(([g]) => g !== "all")
    .sort((a, b) => Number(isBuiltinBucket(a[0])) - Number(isBuiltinBucket(b[0])) || b[1] - a[1]);

  return (
    <div className={styles.root}>
      <div className={styles.col}>
        <div className={styles.head}>
          <Text className={styles.title} block>
            分组
          </Text>
          <Badge appearance="outline">{counts.all ?? 0}</Badge>
        </div>
        <span className={styles.hint}>
          组名可含空格与任意文字；重命名会整组改指，删除只解除归属不删记录。
        </span>
        {groups.length === 0 && (
          <EmptyState text="暂无分组：复制内容后分类器给出建议，采纳后才写入分组。" loading={!loaded} />
        )}
        {groups.map(([group, n]) => (
          <div className={styles.row} key={group}>
            {editing === group ? (
              <>
                <Input
                  size="small"
                  aria-label="新组名"
                  value={draft}
                  onChange={(_, d) => setDraft(d.value)}
                  onKeyDown={(ev) => {
                    if (ev.key === "Enter") void commitRename();
                    if (ev.key === "Escape") setEditing(null);
                  }}
                  style={{ flex: 1, minWidth: 0 }}
                />
                <Button size="small" appearance="primary" onClick={() => void commitRename()}>
                  确定
                </Button>
                <Button size="small" onClick={() => setEditing(null)}>
                  取消
                </Button>
              </>
            ) : (
              <>
                <span className={styles.name}>
                  {GROUP_LABEL[group] ?? group}
                  {GROUP_LABEL[group] && <span className={styles.builtin}> · {group}</span>}
                </span>
                <Badge appearance="tint" color={isBuiltinBucket(group) ? "informative" : "brand"}>
                  {n}
                </Badge>
                {isBuiltinBucket(group) ? (
                  <span className={styles.builtin}>内置筛选</span>
                ) : (
                  <div className={styles.ops}>
                    <Tooltip content="整组重命名" relationship="label">
                      <Button size="small" disabled={busy} onClick={() => startRename(group)}>
                        重命名
                      </Button>
                    </Tooltip>
                    <Tooltip content="解除归属（记录保留）" relationship="label">
                      <Button size="small" disabled={busy} onClick={() => void doDeleteGroup(group)}>
                        删除
                      </Button>
                    </Tooltip>
                  </div>
                )}
              </>
            )}
          </div>
        ))}
      </div>

      <div className={styles.col}>
        <div className={styles.head}>
          <Text className={styles.title} block>
            建议归组
          </Text>
          <Badge appearance="outline">{sugg.length}</Badge>
        </div>
        <span className={styles.hint}>
          分类器只提建议，永不自动改分组：采纳才写入，忽略只是不再提示。
        </span>
        {sugg.length === 0 && (
          <EmptyState text="没有待处理的建议：未分组条目的分类结果会出现在这里。" loading={!loaded} />
        )}
        {sugg.map((s) => (
          <div className={styles.card} key={s.entry_id}>
            <span className={styles.preview}>{s.preview}</span>
            <span className={styles.meta}>
              建议归入「{GROUP_LABEL[s.suggested_group] ?? s.suggested_group}」· 置信度{" "}
              {(s.confidence * 100).toFixed(0)}%
            </span>
            <div className={styles.ops}>
              <Button
                size="small"
                appearance="primary"
                disabled={busy}
                onClick={() => void doApply(s, true)}
              >
                采纳
              </Button>
              <Button size="small" disabled={busy} onClick={() => void doApply(s, false)}>
                忽略
              </Button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
