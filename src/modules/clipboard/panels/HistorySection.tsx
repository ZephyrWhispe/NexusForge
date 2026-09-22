import { useCallback, useEffect, useRef, useState } from "react";
import {
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  makeStyles,
  Text,
  tokens,
  Tooltip,
} from "@fluentui/react-components";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  clipboardSearch,
  clipboardPaste,
  clipboardPin,
  clipboardDelete,
  clipboardClear,
  clipboardGet,
  clipboardGroupCounts,
  clipboardEntrySetGroup,
  clipboardCaptureGet,
  clipboardStackPush,
  parseAppError,
  type ClipEntry,
  type ClipCaptureState,
  type ClipSearchQuery,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify, reportError } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";
import EmptyState from "../../../components/EmptyState";
import DibThumb from "../DibThumb";
import { keyActivate } from "../../../a11y";
import { GROUP_LABEL, fmtTime } from "../display";

/**
 * 历史子面板（T-B3-1 自 ClipboardPanel 原样搬入，逻辑一行不删）：
 * 数据 clipboard_search FTS/分页；实时 nf:event clipboard.captured/deleted/cleared → 刷新；
 * 行操作（详情/粘贴/置顶/删除）+ 工具栏清空 + 两个 Dialog 形态均沿用 B1 判据。
 */
const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    height: "40px",
    padding: "0 20px",
    flexShrink: 0,
  },
  // 破坏性主按钮 = 红色（与 ConfirmDialog 同基准，当前 Fluent 版本无 overflow appearance）
  danger: {
    backgroundColor: tokens.colorPaletteRedBackground1,
    color: tokens.colorPaletteRedForeground1,
  },
  impact: {
    display: "block",
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
  },
  detailHint: {
    display: "block",
    marginTop: "6px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  fullText: {
    display: "block",
    maxHeight: "46vh",
    overflowY: "auto",
    margin: "4px 0 0",
    padding: "8px 12px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
  },
  meta: { display: "block", marginBottom: "6px", fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2 },
  banner: {
    display: "block",
    margin: "0 20px 6px",
    padding: "7px 12px",
    borderRadius: tokens.borderRadiusMedium,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorPaletteDarkOrangeForeground1,
    backgroundColor: tokens.colorPaletteDarkOrangeBackground1,
    flexShrink: 0,
  },
  list: { flex: 1, overflowY: "auto", padding: "4px 20px 20px" },
  entry: {
    display: "flex",
    gap: "12px",
    alignItems: "flex-start",
    padding: "11px 14px",
    borderRadius: tokens.borderRadiusLarge,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    cursor: "pointer",
    ":hover": { backgroundColor: tokens.colorNeutralBackground1Hover },
  },
  body: { flex: 1, minWidth: 0 },
  row1: { display: "flex", alignItems: "center", gap: "8px", marginBottom: "3px" },
  src: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2 },
  time: { fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3, marginLeft: "auto" },
  chip: {
    fontSize: tokens.fontSizeBase100,
    padding: "1px 7px",
    borderRadius: tokens.borderRadiusCircular,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    color: tokens.colorNeutralForeground2,
  },
  chipSecret: {
    color: tokens.colorPaletteDarkOrangeForeground1,
    border: `1px solid ${tokens.colorPaletteDarkOrangeForeground1}`,
  },
  chipPin: {
    color: tokens.colorBrandForeground1,
    border: `1px solid ${tokens.colorBrandForeground1}`,
  },
  preview: {
    color: tokens.colorNeutralForeground2,
    fontSize: tokens.fontSizeBase300,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  mono: { fontFamily: tokens.fontFamilyMonospace, fontSize: tokens.fontSizeBase200 },
  ops: {
    display: "flex",
    gap: "2px",
    opacity: 0,
    transitionProperty: "opacity",
    transitionDuration: "120ms",
    ":hover": { opacity: 1 },
  },
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
});

/** 查询参数构造（纯函数，Vitest 覆盖）：空搜索不参与过滤；"all" 分组不进 SQL；分页页码原样下传 */
export function clipSearchParams(search: string, group: string, page: number, size = 50): ClipSearchQuery {
  return {
    text: search || undefined,
    group: group !== "all" ? group : undefined,
    page,
    size,
  };
}

interface Props {
  search: string;
  group: string;
  onCounts: (counts: Record<string, number>) => void;
}

/** 图片条目缩略图：见 DibThumb.tsx（与 QuickPanel 共用） */

export default function HistorySection({ search, group, onCounts }: Props) {
  const styles = useStyles();
  const [entries, setEntries] = useState<ClipEntry[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [page, setPage] = useState(0);
  // 首轮查询是否已落定（成功或失败）：未落定前不渲染引导文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  // T-B1-1 清空：面板局部 Dialog 携带「保留置顶」选项（全局 confirmAction 只回布尔、无选项通道）
  const [clearOpen, setClearOpen] = useState(false);
  const [keepPinned, setKeepPinned] = useState(true);
  const [clearing, setClearing] = useState(false);
  // T-B1-2 详情：text/files 走 clipboard_get 全文；image 走大图 DibThumb（字节不做 lossy 展示）
  const [detail, setDetail] = useState<{ entry: ClipEntry; text: string | null; failed: string | null } | null>(
    null,
  );
  // T-B3-4 详情内的手工分组草稿（打开详情时以条目现值播种）
  const [groupDraft, setGroupDraft] = useState("");
  // T-B3-2 暂停捕获横幅：读运行态（与设置卡/托盘同源），只在真跳过过内容时出现
  const [capture, setCapture] = useState<ClipCaptureState | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const load = useCallback(
    async (p: number, append: boolean) => {
      try {
        const res = await clipboardSearch(clipSearchParams(search, group, p));
        setEntries((prev) => (append ? [...prev, ...res.items] : res.items));
        setHasMore(res.has_more);
        setPage(p);
      } finally {
        setLoaded(true);
      }
    },
    [search, group],
  );

  // U3-3：分组计数（每次数据刷新后同步）
  const refreshCounts = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardGroupCounts()
      .then((c) => onCounts(c as Record<string, number>))
      .catch((e) => reportError(e, { context: "分组计数刷新失败", dedupeKey: "clip-counts", toast: false }));
  }, [onCounts]);

  // 搜索/分组变化 → 重置首页
  useEffect(() => {
    load(0, false).catch(() => notify("error", "加载失败", "模块未就绪或 DB 异常"));
    refreshCounts();
  }, [load, refreshCounts]);

  const refreshCapture = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardCaptureGet()
      .then(setCapture)
      .catch((e) => reportError(e, { context: "捕获状态刷新失败", dedupeKey: "clip-capture-state", toast: false }));
  }, []);

  useEffect(refreshCapture, [refreshCapture]);

  // U3-6 实时更新：clipboard.captured → 刷新首页
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen("nf:event", (e) => {
        const topic = (e.payload as { topic?: string }).topic;
        if (topic === "clipboard.captured")
          load(0, false).catch((e) =>
            reportError(e, { context: "剪切板实时刷新失败", dedupeKey: "clip-event-refresh", toast: false }),
          );
        if (
          topic === "clipboard.deleted" ||
          topic === "clipboard.cleared" ||
          topic === "clipboard.captured"
        )
          refreshCounts();
        if (topic === "clipboard.capture_state") refreshCapture();
      }),
    ).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
    };
  }, [load, refreshCounts, refreshCapture]);

  // U3-1 虚拟列表：64px 仅作初估，实际行高由 measureElement 动态测量
  //（图片行含 52px 缩略图 + 上下 padding ≈75px，固定行高会重叠）
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 64,
    overscan: 10,
  });

  const doPaste = (e: ClipEntry) =>
    clipboardPaste(e.id)
      .then(() => notify("success", "已写入剪贴板", "回写窗口 500ms 内不重复记录"))
      .catch(() => notify("error", "粘贴失败"));
  const doPin = (e: ClipEntry) =>
    clipboardPin(e.id, !e.pinned)
      .then(() => load(page, false))
      .catch(() => notify("error", "置顶失败"));

  // T-B3-3 入栈：栈深由返回值如实报出；重复入栈幂等（后端 WHERE NOT EXISTS 兜底）
  const doPushStack = (e: ClipEntry) =>
    clipboardStackPush(e.id)
      .then((depth) => notify("success", "已加入粘贴堆栈", `当前队列 ${depth} 条`))
      .catch((err) =>
        notify("error", "入栈失败", parseAppError(err)?.data.message ?? String(err)),
      );

  // 行内删除（D-18）：单击即删改为经全局 ConfirmDialog，影响面点名到条目
  const doDelete = async (e: ClipEntry) => {
    const raw = e.preview.trim();
    const preview = raw.length > 40 ? `${raw.slice(0, 40)}…` : raw;
    if (
      !(await confirmAction({
        title: "删除剪贴板记录",
        impact: [
          "将删除 1 条剪贴板记录",
          e.secret ? "敏感条目（加密信封，内容不在此显示）" : `内容预览：${preview || "（无预览）"}`,
          e.pinned ? "该条目为置顶收藏" : "",
        ].filter(Boolean),
        detail: "删除后不可恢复；图片条目的 blob 与敏感条目的密文一并清理。",
        confirmLabel: "删除",
      }))
    )
      return;
    try {
      await clipboardDelete(e.id);
      notify("success", "已删除 1 条记录");
      await load(0, false);
    } catch {
      notify("error", "删除失败");
    }
  };

  // T-B1-1：实删条数以 clipboard_clear 返回值如实回报；列表重载由本处理器自调
  //（clipboard.cleared 订阅只做分组计数刷新，不动它）
  const doClear = async () => {
    setClearing(true);
    try {
      const removed = await clipboardClear(keepPinned);
      notify(
        "success",
        "已清空剪切板历史",
        `删除 ${removed} 条记录${keepPinned ? "，置顶条目已保留" : ""}`,
      );
      setClearOpen(false);
      await load(0, false);
    } catch (e) {
      notify("error", "清空失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setClearing(false);
    }
  };

  const openDetail = (e: ClipEntry) => {
    setDetail({ entry: e, text: null, failed: null });
    setGroupDraft(e.group ?? "");
    if (e.content_type === "image") return;
    clipboardGet(e.id)
      .then((t) => setDetail((d) => (d && d.entry.id === e.id ? { ...d, text: t } : d)))
      .catch((err) =>
        setDetail((d) =>
          d && d.entry.id === e.id
            ? { ...d, failed: parseAppError(err)?.data.message ?? String(err) }
            : d,
        ),
      );
  };

  /** T-B3-4 手工分组：留空 = 取消分组（写口 clipboard_entry_set_group 单点） */
  const saveGroup = async (id: string) => {
    const name = groupDraft.trim();
    try {
      await clipboardEntrySetGroup(id, name === "" ? null : name);
      setDetail((d) => (d ? { ...d, entry: { ...d.entry, group: name === "" ? null : name } } : d));
      notify("success", "分组已更新", name === "" ? "该条目回到未分组" : `「${name}」`);
      refreshCounts();
    } catch (err) {
      notify("error", "改分组失败", parseAppError(err)?.data.message ?? String(err));
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Button size="small" onClick={() => setClearOpen(true)}>
          清空
        </Button>
      </div>
      {capture && capture.skipped > 0 && (
        <span className={styles.banner}>暂停期间已跳过 {capture.skipped} 次复制</span>
      )}
      <div className={styles.list} ref={listRef}>
      {entries.length === 0 ? (
        <EmptyState
          text="没有匹配的记录：复制任意内容后这里会显示历史（文本实时捕获）。"
          loading={!loaded}
        />
      ) : (
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((vi) => {
            const e = entries[vi.index];
            const isCode = e.group === "code" || e.group === "json";
            return (
              <div
                key={e.id}
                data-index={vi.index}
                ref={virtualizer.measureElement}
                className={styles.entry}
                style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${vi.start}px)` }}
                onClick={() => doPaste(e)}
                role="button"
                tabIndex={0}
                onKeyDown={keyActivate(() => doPaste(e))}
              >
                {e.content_type === "image" && <DibThumb id={e.id} />}
                <div className={styles.body}>
                  <div className={styles.row1}>
                    {e.pinned && <span className={`${styles.chip} ${styles.chipPin}`}>★ 置顶</span>}
                    {e.secret ? (
                      <span className={`${styles.chip} ${styles.chipSecret}`}>已加密</span>
                    ) : (
                      e.group && <span className={styles.chip}>{GROUP_LABEL[e.group] ?? e.group}</span>
                    )}
                    {e.origin === "remote" && (
                      <span className={styles.chip}>远端</span>
                    )}
                    {e.source_app && (
                      <span className={styles.src}>{e.source_app}</span>
                    )}
                    <span className={styles.time}>{fmtTime(e.created_at)}</span>
                  </div>
                  <div className={`${styles.preview} ${isCode ? styles.mono : ""}`}>{e.preview}</div>
                </div>
                <div className={styles.ops}>
                  <Tooltip content="详情" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        openDetail(e);
                      }}
                      aria-label="详情"
                    >
                      ⓘ
                    </button>
                  </Tooltip>
                  <Tooltip content="粘贴" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        doPaste(e);
                      }}
                      aria-label="粘贴"
                    >
                      ⏎
                    </button>
                  </Tooltip>
                  <Tooltip content="加入粘贴堆栈" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        void doPushStack(e);
                      }}
                      aria-label="入栈"
                    >
                      ⛶
                    </button>
                  </Tooltip>
                  <Tooltip content="置顶" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        doPin(e);
                      }}
                      aria-label="置顶"
                    >
                      ☆
                    </button>
                  </Tooltip>
                  <Tooltip content="删除" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        void doDelete(e);
                      }}
                      aria-label="删除"
                    >
                      ✕
                    </button>
                  </Tooltip>
                </div>
              </div>
            );
          })}
        </div>
      )}
      {hasMore && (
        <div style={{ textAlign: "center", padding: "12px" }}>
          <button
            className={styles.opBtn}
            style={{ width: "auto", padding: "0 14px", border: `1px solid ${tokens.colorNeutralStroke1}` }}
            onClick={() =>
              void load(page + 1, true).catch(() => notify("error", "加载失败", "模块未就绪或 DB 异常"))
            }
          >
            加载更多
          </button>
        </div>
      )}
      </div>

      {/* T-B1-1 清空二次确认（D-18 基线形态；Checkbox 走局部 Dialog 承载，见 09 §4.2 行内更正） */}
      <Dialog open={clearOpen} onOpenChange={(_, d) => !d.open && setClearOpen(false)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>清空剪切板历史</DialogTitle>
            <DialogContent>
              <span className={styles.impact}>
                {keepPinned ? "将删除当前库中全部未置顶记录" : "将删除当前库中全部记录（含置顶）"}
              </span>
              <span className={styles.detailHint}>
                删除后不可恢复；图片条目的 blob 与敏感条目的密文一并清理。实际删除条数以执行结果提示为准。
              </span>
              <Checkbox
                checked={keepPinned}
                onChange={(_, d) => setKeepPinned(d.checked === true)}
                label="保留置顶条目"
                style={{ marginTop: "10px" }}
              />
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setClearOpen(false)} disabled={clearing}>
                取消
              </Button>
              <Button
                appearance="primary"
                className={styles.danger}
                disabled={clearing}
                onClick={() => void doClear()}
              >
                {clearing ? "清空中…" : "确认清空"}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* T-B1-2 行详情：全文经 clipboard_get；空串=不存在/空内容合一，如实并陈 */}
      <Dialog open={detail !== null} onOpenChange={() => setDetail(null)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>记录详情</DialogTitle>
            <DialogContent>
              {detail && (
                <>
                  <Text className={styles.meta}>
                    {[
                      detail.entry.source_app || "未知来源",
                      fmtTime(detail.entry.created_at),
                      detail.entry.pinned ? "置顶" : "",
                      detail.entry.secret ? "已加密" : "",
                      detail.entry.origin === "remote" ? "远端" : "",
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </Text>
                  {detail.entry.content_type === "image" ? (
                    <DibThumb id={detail.entry.id} width={420} height={280} />
                  ) : detail.failed ? (
                    <span className={styles.detailHint}>读取失败：{detail.failed}</span>
                  ) : detail.text === null ? (
                    <span className={styles.detailHint}>加载中…</span>
                  ) : detail.text === "" ? (
                    <span className={styles.detailHint}>内容已空或记录已删</span>
                  ) : (
                    <pre className={styles.fullText}>{detail.text}</pre>
                  )}
                  {/* T-B3-4 手工分组：组名任意 Unicode；留空保存即取消分组 */}
                  <div style={{ display: "flex", gap: "8px", marginTop: "12px" }}>
                    <Input
                      size="small"
                      aria-label="分组名"
                      placeholder={GROUP_LABEL[detail.entry.group ?? ""] ?? "未分组"}
                      value={groupDraft}
                      onChange={(_, d) => setGroupDraft(d.value)}
                      style={{ flex: 1, minWidth: 0 }}
                    />
                    <Button
                      size="small"
                      disabled={groupDraft.trim() === (detail.entry.group ?? "")}
                      onClick={() => void saveGroup(detail.entry.id)}
                    >
                      保存分组
                    </Button>
                  </div>
                </>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setDetail(null)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
