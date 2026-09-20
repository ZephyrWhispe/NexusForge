import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
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
  desktopLauncherReindex,
  desktopLauncherStatus,
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
import { useDesktopReminders } from "../../stores/desktopReminders";
import { confirmAction } from "../../stores/confirm";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 桌面效率面板（docs/impl/05 D3+D4，M8 v1）：
 * ① 随记管理（列表/新增/完成/删除）② 桌面整理（预览→应用→还原）
 * ③ 启动器索引重建（D1：core 内 build 后重放内置动作）。
 * 提醒到期横幅读 `useDesktopReminders` 缓冲（订阅在 MainWorkbench 级，面板外事件不丢）；
 * 刻意不调 `desktop_notes_due`——该命令 take_due 是破坏性消费，会抢走后台轮询的事件。
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
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  grow: { flex: 1, minWidth: "240px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
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
  // D-18：成功提示与错误分离（旧实现把"整理完成"塞进 setError，随即被 run 的 setError("") 抹掉）
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState("");
  const due = useDesktopReminders((s) => s.due);
  const dismissDue = useDesktopReminders((s) => s.dismissDue);
  // 首轮 refresh 是否落定：未落定前随记列表渲染加载态而非"暂无随记"（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
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
        desktopLauncherStatus(),
      ]);
      if (!mounted.current) return;
      setHasManifest(tidyStatus);
      setPlan(tidyPlan);
      setIndexReady(idx);
      await refreshNotes();
      setError("");
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setLoaded(true);
    }
  }, [refreshNotes]);

  // 提醒事件订阅已上移到 MainWorkbench 级 feed（startDesktopRemindFeed），
  // 面板只在关闭时也不丢缓冲；这里不再自行 listen。
  useEffect(() => {
    mounted.current = true;
    void refresh();
  }, [refresh]);

  const run = useCallback(async (key: string, action: () => Promise<unknown>) => {
    setBusy(key);
    setMsg("");
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

  // 删除随记（D-18 破坏性操作）：不可恢复，确认框点名内容
  const removeNote = (n: DesktopNoteDto) =>
    void (async () => {
      const preview = n.content.length > 40 ? `${n.content.slice(0, 40)}…` : n.content;
      if (
        !(await confirmAction({
          title: "删除随记",
          impact: [`将删除 1 条随记`, `「${preview}」`],
          detail: n.remind_at && !n.done ? "该随记的到期提醒将一并取消，删除后不可恢复。" : "删除后不可恢复。",
          confirmLabel: "删除",
        }))
      )
        return;
      await run(`del-${n.id}`, async () => {
        await desktopNoteRemove(n.id);
        await refreshNotes();
      });
    })();

  // 一键整理（D-18）：移动桌面文件属批量改动但可还原（生成快照）→ danger=false，量化 plan.total
  const applyTidy = () =>
    void (async () => {
      if (!plan) return;
      if (
        !(await confirmAction({
          title: "一键整理桌面",
          impact: `将重排 ${plan.total} 个桌面图标`,
          detail: "按类型归入桌面分类文件夹，并创建还原快照；快捷方式与文件夹不动，整理后可随时「还原」。",
          danger: false,
          confirmLabel: "开始整理",
        }))
      )
        return;
      await run("apply", async () => {
        const [moved, skipped] = await desktopTidyApply();
        await refresh();
        if (mounted.current) {
          setMsg(
            skipped > 0
              ? `整理完成：移动 ${moved} 个，跳过 ${skipped} 个（同名/占用）`
              : `整理完成：移动 ${moved} 个桌面图标`,
          );
        }
      });
    })();

  // 还原（D-18）：撤销上次整理、把快照内文件移回原位；还原后该快照删除 → danger=false
  const restoreTidy = () =>
    void (async () => {
      if (
        !(await confirmAction({
          title: "还原桌面布局",
          impact: "将按还原快照（1 份）把上次整理移动的文件移回桌面原位",
          detail: "撤销上一次一键整理：分类文件夹内的文件回到桌面；快捷方式与文件夹不受影响。还原后该快照将被删除，需重新整理才会生成新的。",
          danger: false,
          confirmLabel: "还原",
        }))
      )
        return;
      await run("restore", async () => {
        await desktopTidyRestore();
        await refresh();
        if (mounted.current) setMsg("已还原桌面整理前的布局快照");
      });
    })();

  // 启动器索引重建（T-B1-6）：core 内 build 会整体替换条目并重放内置动作，
  // 返回数是 App 条目（不含动作），徽标以重建后的 status 合计为准，两处数字不混用。
  const doReindex = () =>
    run("reindex", async () => {
      const apps = await desktopLauncherReindex();
      const idx = await desktopLauncherStatus();
      if (mounted.current) {
        setIndexReady(idx);
        setMsg(`索引已重建：应用 ${apps} 条 + 内置动作（当前合计 ${idx[1]} 条）`);
      }
    });

  return (
    <div className={styles.root}>
      {due.length > 0 && (
        <Section>
          <div className={styles.row}>
            <Badge appearance="filled" color="warning">提醒</Badge>
            <span className={styles.remind}>{due[0].content}</span>
            {due.length > 1 && (
              <Badge appearance="outline" color="warning">缓冲 {due.length} 条</Badge>
            )}
            <span className={styles.grow} />
            <Button size="small" onClick={() => dismissDue(due[0].id)}>知道了</Button>
          </div>
        </Section>
      )}

      {/* 随记（D4） */}
      <Section title="待办与随记">
        <span className={styles.muted}>
          全局 Ctrl+Alt+N 呼出速记条；支持 #标签、“明天/周几 X点”提醒
        </span>
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
          <EmptyState text="暂无随记——在上方输入框记录待办，支持 #标签 与提醒" loading={!loaded} />
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
              <Button size="small" disabled={busy !== ""} onClick={() => removeNote(n)}>
                删除
              </Button>
            </div>
          ))
        )}
      </Section>

      {/* 桌面整理（D3） */}
      <Section
        title="桌面整理"
        actions={
          <>
            {hasManifest && <Badge appearance="outline" color="warning">有可还原记录</Badge>}
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
              onClick={applyTidy}
            >
              {busy === "apply" ? "整理中…" : "一键整理"}
            </Button>
            <Button size="small" disabled={busy !== "" || !hasManifest} onClick={restoreTidy}>
              还原
            </Button>
          </>
        }
      >
        {plan && plan.total === 0 ? (
          <EmptyState text="桌面没有待整理的普通文件（快捷方式与文件夹不动）" />
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
      </Section>

      {/* 启动器（D1/D2 状态说明 + 索引重建入口） */}
      <Section
        title="快速启动器"
        actions={
          <>
            <Button size="small" disabled={busy !== ""} onClick={doReindex}>
              {busy === "reindex" ? "重建中…" : "重建索引"}
            </Button>
            {indexReady[0] ? (
              <Badge appearance="outline" color="success">索引就绪 · {indexReady[1]} 条</Badge>
            ) : (
              <Badge appearance="outline">索引构建中</Badge>
            )}
          </>
        }
      >
        <span className={styles.muted}>
          全局 Alt+Q 呼出；打分 = 前缀命中 0.5 + 子序列连续度 0.3 + 频次衰减 0.2。
          新装软件未出现在启动器时点「重建索引」重新扫描开始菜单与 PATH（内置快捷动作会自动恢复）。
        </span>
      </Section>

      <InlineError text={msg} tone="success" />
      <InlineError text={error} />
    </div>
  );
}
