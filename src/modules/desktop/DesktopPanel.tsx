import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Checkbox,
  Table,
  TableBody,
  TableCell,
  TableRow,
} from "@fluentui/react-components";
import {
  desktopNoteAdd,
  desktopNoteDone,
  desktopNoteList,
  desktopNoteRemove,
  desktopTidyApply,
  desktopTidyPlan,
  desktopTidyRestore,
  desktopTidyStatus,
  parseAppError,
  type DesktopNoteDto,
  type DesktopTidyPlanDto,
} from "../../ipc/client";

/**
 * 桌面效率面板（docs/impl/05 D3+D4，M8 v1）：
 * ① 随记管理（列表/新增/完成/删除；提醒到期由 desktop.remind_due 事件+轮询提示）
 * ② 桌面整理（预览分类 → 应用 → 一键还原；lnk/目录不动）
 * 启动器（D1/D2）为全局 Alt+Q 独立窗口，不内嵌。
 */
const useStyles = makeStyles({
  root: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    padding: "0 20px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "16px",
  },
  section: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "12px 16px",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexDirection: "column",
    gap: "10px",
  },
  sectionHead: { display: "flex", alignItems: "center", gap: "8px" },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  grow: { flex: 1, minWidth: "240px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  tag: {
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorBrandForeground1,
    marginRight: "4px",
  },
  remind: { color: tokens.colorPaletteMarigoldForeground1, fontSize: tokens.fontSizeBase200 },
  noteRow: {
    display: "flex",
    alignItems: "flex-start",
    gap: "8px",
    padding: "6px 0",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  noteContent: {
    flex: 1,
    minWidth: 0,
    whiteSpace: "pre-wrap",
    wordBreak: "break-word",
  },
  done: { textDecoration: "line-through", color: tokens.colorNeutralForeground3 },
  groupHead: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "4px 0",
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase300,
  },
});

const fmtTime = (ms: number) => new Date(ms).toLocaleString();

const fmtRemind = (ms: number | null) =>
  ms ? `⏰ ${new Date(ms).toLocaleString()}` : "";

export default function DesktopPanel() {
  const styles = useStyles();
  const [notes, setNotes] = useState<DesktopNoteDto[]>([]);
  const [showDone, setShowDone] = useState(false);
  const [input, setInput] = useState("");
  const [plan, setPlan] = useState<DesktopTidyPlanDto | null>(null);
  const [hasManifest, setHasManifest] = useState(false);
  const [indexReady, setIndexReady] = useState<[boolean, number]>([false, 0]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [dueNote, setDueNote] = useState<DesktopNoteDto | null>(null);
  const mounted = useRef(true);

  const refreshNotes = useCallback(async () => {
    try {
      const list = await desktopNoteList(showDone);
      if (mounted.current) setNotes(list);
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    }
  }, [showDone]);

  const refresh = useCallback(async () => {
    try {
      const [tidyStatus, tidyPlan, idx] = await Promise.all([
        desktopTidyStatus(),
        desktopTidyPlan(),
        import("../../ipc/client").then((c) => c.desktopLauncherStatus()),
      ]);
      if (!mounted.current) return;
      setHasManifest(tidyStatus);
      setPlan(tidyPlan);
      setIndexReady(idx);
      await refreshNotes();
      setError("");
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    }
  }, [refreshNotes]);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    // 提醒到期事件 → 顶部提示条（事件驱动，30s 轮询由后端负责）
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          if ((e.payload as { topic?: string }).topic !== "desktop.remind_due") return;
          const p = (e.payload as { payload?: DesktopNoteDto }).payload;
          if (p && mounted.current) {
            setDueNote(p);
            void refreshNotes();
          }
        }),
      )
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refresh, refreshNotes]);

  const run = useCallback(async (key: string, action: () => Promise<unknown>) => {
    setBusy(key);
    try {
      await action();
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setBusy("");
    }
  }, []);

  const addNote = () =>
    run("note-add", async () => {
      await desktopNoteAdd(input);
      setInput("");
      await refreshNotes();
    });

  return (
    <div className={styles.root}>
      {dueNote && (
        <div className={styles.section}>
          <div className={styles.sectionHead}>
            <Badge appearance="filled" color="warning">提醒</Badge>
            <span className={styles.remind}>{dueNote.content}</span>
            <span className={styles.grow} />
            <Button size="small" onClick={() => setDueNote(null)}>知道了</Button>
          </div>
        </div>
      )}

      {/* 随记（D4） */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">待办与随记</Text>
          <span className={styles.muted}>
            全局 Ctrl+Alt+N 呼出速记条；支持 #标签、“明天/周几 X点”提醒
          </span>
        </div>
        <div className={styles.row}>
          <Input
            className={styles.grow}
            placeholder="例如：明天下午3点 交周报 #工作"
            value={input}
            onChange={(_, d) => setInput(d.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && input.trim()) void addNote();
            }}
            size="small"
          />
          <Button
            size="small"
            appearance="primary"
            disabled={busy !== "" || input.trim() === ""}
            onClick={addNote}
          >
            添加
          </Button>
          <Checkbox
            label="显示已完成"
            checked={showDone}
            onChange={(_, d) => setShowDone(d.checked === true)}
          />
        </div>
        {notes.length === 0 ? (
          <span className={styles.muted}>暂无随记</span>
        ) : (
          notes.map((n) => (
            <div key={n.id} className={styles.noteRow}>
              <Checkbox
                checked={n.done}
                onChange={(_, d) =>
                  run(`done-${n.id}`, async () => {
                    await desktopNoteDone(n.id, d.checked === true);
                    await refreshNotes();
                  })
                }
              />
              <div className={styles.noteContent}>
                <div className={n.done ? styles.done : undefined}>{n.content}</div>
                <div>
                  {n.tags.map((t) => (
                    <span key={t} className={styles.tag}>#{t}</span>
                  ))}
                  {n.remind_at && <span className={styles.remind}>{fmtRemind(n.remind_at)}</span>}
                  <span className={styles.muted}> · {fmtTime(n.created_ms)}</span>
                </div>
              </div>
              <Button
                size="small"
                disabled={busy !== ""}
                onClick={() =>
                  run(`del-${n.id}`, async () => {
                    await desktopNoteRemove(n.id);
                    await refreshNotes();
                  })
                }
              >
                删除
              </Button>
            </div>
          ))
        )}
      </div>

      {/* 桌面整理（D3） */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">桌面整理</Text>
          {hasManifest && <Badge appearance="outline" color="warning">有可还原记录</Badge>}
          <span className={styles.grow} />
          <Button
            size="small"
            disabled={busy !== ""}
            onClick={() =>
              run("plan", async () => {
                setPlan(await desktopTidyPlan());
              })
            }
          >
            刷新预览
          </Button>
          <Button
            size="small"
            appearance="primary"
            disabled={busy !== "" || !plan || plan.total === 0}
            onClick={() =>
              run("apply", async () => {
                const [moved, skipped] = await desktopTidyApply();
                await refresh();
                if (mounted.current) {
                  setError(
                    skipped > 0 ? `整理完成：移动 ${moved} 个，跳过 ${skipped} 个（同名/占用）` : "",
                  );
                }
              })
            }
          >
            {busy === "apply" ? "整理中…" : "一键整理"}
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || !hasManifest}
            onClick={() =>
              run("restore", async () => {
                await desktopTidyRestore();
                await refresh();
              })
            }
          >
            还原
          </Button>
        </div>
        {plan && plan.total === 0 ? (
          <span className={styles.muted}>桌面没有待整理的普通文件（快捷方式与文件夹不动）</span>
        ) : (
          plan?.groups.map(([cat, items]) => (
            <div key={cat}>
              <div className={styles.groupHead}>
                {cat} <Badge appearance="outline">{items.length}</Badge>
              </div>
              <Table size="small">
                <TableBody>
                  {items.slice(0, 8).map((it) => (
                    <TableRow key={it.path}>
                      <TableCell>{it.name}</TableCell>
                    </TableRow>
                  ))}
                  {items.length > 8 && (
                    <TableRow key="__more">
                      <TableCell>
                        <span className={styles.muted}>… 共 {items.length} 个</span>
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </div>
          ))
        )}
      </div>

      {/* 启动器（D1/D2 状态说明） */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">快速启动器</Text>
          {indexReady[0] ? (
            <Badge appearance="outline" color="success">索引就绪 · {indexReady[1]} 条</Badge>
          ) : (
            <Badge appearance="outline">索引构建中</Badge>
          )}
        </div>
        <span className={styles.muted}>
          全局 Alt+Q 呼出；打分 = 前缀命中 0.5 + 子序列连续度 0.3 + 频次衰减 0.2。
        </span>
      </div>

      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
}
