import { useCallback, useEffect, useState } from "react";
import {
  Button,
  Input,
  makeStyles,
  Text,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  clipboardStackClear,
  clipboardStackList,
  clipboardStackMove,
  clipboardStackPasteAll,
  clipboardStackPasteNext,
  clipboardStackRemove,
  parseAppError,
  type ClipEntry,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify, reportError } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";
import EmptyState from "../../../components/EmptyState";
import { fmtTime } from "../display";

/**
 * 粘贴堆栈子面板（T-B3-3，细案 01§3-10/11 + §7.1）：队列编辑器视图。
 * 投递前必须让出焦点（先隐藏本窗，注入 Ctrl+V 后恢复），否则粘贴落在自己身上。
 * 敏感条目留在队首即挡住队列——这是出栈纪律，面板如实显示原因不静默跳过。
 */
const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "0 20px",
    height: "48px",
    flexShrink: 0,
  },
  hint: {
    marginLeft: "auto",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  danger: {
    backgroundColor: tokens.colorPaletteRedBackground1,
    color: tokens.colorPaletteRedForeground1,
  },
  list: { flex: 1, overflowY: "auto", padding: "4px 20px 20px" },
  row: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    padding: "10px 12px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
  },
  pos: {
    flex: "none",
    width: "22px",
    height: "22px",
    borderRadius: tokens.borderRadiusSmall,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    display: "grid",
    placeItems: "center",
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground2,
  },
  preview: {
    flex: 1,
    minWidth: 0,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground2,
  },
  chip: {
    fontSize: tokens.fontSizeBase100,
    padding: "1px 7px",
    borderRadius: tokens.borderRadiusCircular,
    border: `1px solid ${tokens.colorPaletteDarkOrangeForeground1}`,
    color: tokens.colorPaletteDarkOrangeForeground1,
  },
  time: { fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3 },
  ops: { display: "flex", gap: "2px" },
  opBtn: {
    width: "30px",
    height: "30px",
    borderRadius: tokens.borderRadiusMedium,
    display: "grid",
    placeItems: "center",
    color: tokens.colorNeutralForeground2,
    backgroundColor: "transparent",
    border: "none",
    cursor: "pointer",
    ":hover": { backgroundColor: tokens.colorNeutralBackground3Hover, color: tokens.colorNeutralForeground1 },
  },
  impact: {
    display: "block",
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
  },
});

/** 投递结果 → 提示文案（拒投/注入失败都不算成功，真因点名给用户） */
function pasteNotice(r: Awaited<ReturnType<typeof clipboardStackPasteNext>>) {
  if (!r) return { kind: "info", title: "堆栈已空", body: "没有可粘贴的条目" } as const;
  if (r.delivered) return { kind: "success", title: "已粘贴 1 条", body: "该项已出栈" } as const;
  return { kind: "error", title: "未粘贴，该项仍在栈上", body: r.error ?? "未知原因" } as const;
}

export default function StackSection() {
  const styles = useStyles();
  const [entries, setEntries] = useState<ClipEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [intervalMs, setIntervalMs] = useState(300);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardStackList()
      .then(setEntries)
      .catch((e) => reportError(e, { context: "堆栈读取失败", dedupeKey: "clip-stack-list", toast: false }))
      .finally(() => setLoaded(true));
  }, []);

  useEffect(refresh, [refresh]);

  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen("nf:event", (e) => {
        const topic = (e.payload as { topic?: string }).topic;
        if (topic === "clipboard.stack_changed" || topic === "clipboard.deleted") refresh();
      }),
    ).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refresh]);

  /** 让出焦点 → 投递 → 恢复窗口（hide 必须先于 invoke，否则 Ctrl+V 落进本窗） */
  const withFocusYield = async <T,>(run: () => Promise<T>): Promise<T> => {
    if (!IN_TAURI) return run();
    const win = getCurrentWindow();
    await win.hide();
    try {
      return await run();
    } finally {
      await win.show();
      await win.setFocus();
    }
  };

  const doPasteNext = async () => {
    setBusy(true);
    try {
      const r = await withFocusYield(clipboardStackPasteNext);
      const n = pasteNotice(r);
      notify(n.kind, n.title, n.body);
      refresh();
    } catch (e) {
      notify("error", "粘贴失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  const doPasteAll = async () => {
    setBusy(true);
    try {
      const r = await withFocusYield(() => clipboardStackPasteAll(intervalMs));
      notify(
        r.failed > 0 ? "error" : "success",
        `全部粘贴：成功 ${r.delivered} 条`,
        r.failed > 0
          ? `${r.failed} 条失败已停在此处，剩余 ${r.remaining} 条仍留在栈上`
          : `${r.delivered} 条已按入栈顺序投递完毕`,
      );
      refresh();
    } catch (e) {
      notify("error", "全部粘贴失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  const doMove = async (id: string, to: number) => {
    try {
      await clipboardStackMove(id, to);
      refresh();
    } catch (e) {
      notify("error", "重排失败", parseAppError(e)?.data.message ?? String(e));
    }
  };

  const doRemove = async (id: string) => {
    try {
      await clipboardStackRemove(id);
      refresh();
    } catch (e) {
      notify("error", "移出失败", parseAppError(e)?.data.message ?? String(e));
    }
  };

  const doClear = async () => {
    if (
      !(await confirmAction({
        title: "清空粘贴堆栈",
        impact: [`将移出堆栈中的 ${entries.length} 条队列项`],
        detail: "仅清空队列，历史记录保留（可从历史重新入栈）。",
        confirmLabel: "清空队列",
      }))
    )
      return;
    try {
      const n = await clipboardStackClear();
      notify("success", "堆栈已清空", `移出 ${n} 条队列项`);
      refresh();
    } catch (e) {
      notify("error", "清空失败", parseAppError(e)?.data.message ?? String(e));
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Tooltip content="投递队首：写剪贴板后注入 Ctrl+V，成功后出栈" relationship="label">
          <Button
            size="small"
            appearance="primary"
            disabled={busy || entries.length === 0}
            onClick={() => void doPasteNext()}
          >
            粘贴下一条
          </Button>
        </Tooltip>
        <Input
          size="small"
          type="number"
          min={0}
          max={5000}
          aria-label="粘贴间隔(ms)"
          value={String(intervalMs)}
          onChange={(_, d) => setIntervalMs(Number(d.value) || 0)}
          style={{ width: "104px" }}
        />
        <Tooltip content="逐条投递直到栈空或首个失败即停" relationship="label">
          <Button
            size="small"
            disabled={busy || entries.length === 0}
            onClick={() => void doPasteAll()}
          >
            全部粘贴
          </Button>
        </Tooltip>
        <Text className={styles.hint}>
          {entries.length > 0 ? `队列 ${entries.length} 条` : "队列为空"}
        </Text>
        <Button
          size="small"
          className={styles.danger}
          disabled={busy || entries.length === 0}
          onClick={() => void doClear()}
        >
          清空
        </Button>
      </div>
      <div className={styles.list}>
        {entries.length === 0 && (
          <EmptyState
            text="堆栈为空：在历史行操作点「入栈」把内容排进队列，按入栈顺序逐条粘贴到目标应用。"
            loading={!loaded}
          />
        )}
        {entries.map((e, i) => (
          <div key={e.id} className={styles.row}>
            <span className={styles.pos}>{i + 1}</span>
            <span className={styles.preview}>{e.preview}</span>
            {e.secret && <span className={styles.chip}>敏感·需先揭示</span>}
            <span className={styles.time}>{fmtTime(e.created_at)}</span>
            <div className={styles.ops}>
              <Tooltip content="前移一位" relationship="label">
                <button
                  className={styles.opBtn}
                  aria-label="前移"
                  disabled={i === 0}
                  onClick={() => void doMove(e.id, i - 1)}
                >
                  ↑
                </button>
              </Tooltip>
              <Tooltip content="后移一位" relationship="label">
                <button
                  className={styles.opBtn}
                  aria-label="后移"
                  disabled={i === entries.length - 1}
                  onClick={() => void doMove(e.id, i + 1)}
                >
                  ↓
                </button>
              </Tooltip>
              <Tooltip content="移出队列（历史记录保留）" relationship="label">
                <button
                  className={styles.opBtn}
                  aria-label="移出"
                  onClick={() => void doRemove(e.id)}
                >
                  ✕
                </button>
              </Tooltip>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
