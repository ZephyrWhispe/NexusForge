import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles, tokens, Tooltip } from "@fluentui/react-components";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  clipboardSearch,
  clipboardPaste,
  clipboardPin,
  clipboardDelete,
  clipboardGroupCounts,
  type ClipEntry,
  type ClipSearchQuery,
} from "../../ipc/client";
import { IN_TAURI } from "../../ipc/env";
import { notify, reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import EmptyState from "../../components/EmptyState";
import DibThumb from "./DibThumb";
import { keyActivate } from "../../a11y";

/**
 * 剪切板历史面板（docs/UI-PLAN.md U3-1..U3-4、U3-6）。
 * 数据：clipboard_search FTS/分页；实时：nf:event clipboard.captured → 刷新。
 */
const useStyles = makeStyles({
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

const GROUP_LABEL: Record<string, string> = {
  url: "链接", json: "JSON", code: "代码", color: "颜色", secret: "敏感",
};

function fmtTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

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

export default function ClipboardPanel({ search, group, onCounts }: Props) {
  const styles = useStyles();
  const [entries, setEntries] = useState<ClipEntry[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [page, setPage] = useState(0);
  // 首轮查询是否已落定（成功或失败）：未落定前不渲染引导文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
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
      }),
    ).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
    };
  }, [load, refreshCounts]);

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

  return (
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
  );
}

// 保持 IN_TAURI 引用（浏览器预览时禁用事件订阅）
export const _IN_TAURI = IN_TAURI;
