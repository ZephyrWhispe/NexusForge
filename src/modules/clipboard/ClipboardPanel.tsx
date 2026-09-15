import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles, tokens, Text, Badge, Tooltip } from "@fluentui/react-components";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  clipboardSearch,
  clipboardPaste,
  clipboardPin,
  clipboardDelete,
  clipboardGroupCounts,
  type ClipEntry,
} from "../../ipc/client";
import { IN_TAURI } from "../../ipc/env";
import DibThumb from "./DibThumb";

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
  empty: {
    flex: 1,
    display: "grid",
    placeItems: "center",
    textAlign: "center",
    color: tokens.colorNeutralForeground3,
  },
  toast: {
    position: "fixed",
    right: "16px",
    bottom: "40px",
    padding: "10px 16px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground3,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    boxShadow: tokens.shadow16,
    zIndex: 30,
  },
});

const GROUP_LABEL: Record<string, string> = {
  url: "链接", json: "JSON", code: "代码", color: "颜色", secret: "敏感",
};

function fmtTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
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
  const [toast, setToast] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast(null), 2600);
  }, []);

  const load = useCallback(
    async (p: number, append: boolean) => {
      const res = await clipboardSearch({
        text: search || undefined,
        group: group !== "all" ? group : undefined,
        page: p,
        size: 50,
      });
      setEntries((prev) => (append ? [...prev, ...res.items] : res.items));
      setHasMore(res.has_more);
      setPage(p);
    },
    [search, group],
  );

  // U3-3：分组计数（每次数据刷新后同步）
  const refreshCounts = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardGroupCounts()
      .then((c) => onCounts(c as Record<string, number>))
      .catch(() => {});
  }, [onCounts]);

  // 搜索/分组变化 → 重置首页
  useEffect(() => {
    load(0, false).catch(() => showToast("加载失败：模块未就绪或 DB 异常"));
    refreshCounts();
  }, [load, showToast, refreshCounts]);

  // U3-6 实时更新：clipboard.captured → 刷新首页
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen("nf:event", (e) => {
        const topic = (e.payload as { topic?: string }).topic;
        if (topic === "clipboard.captured") load(0, false).catch(() => {});
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

  // U3-1 虚拟列表（固定行高 64px）
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => 64,
    overscan: 10,
  });

  const doPaste = (e: ClipEntry) =>
    clipboardPaste(e.id)
      .then(() => showToast(`已写入剪贴板（回写窗口 ${"500ms"} 内不重复记录）`))
      .catch(() => showToast("粘贴失败"));
  const doPin = (e: ClipEntry) =>
    clipboardPin(e.id, !e.pinned)
      .then(() => load(page, false))
      .catch(() => showToast("置顶失败"));
  const doDelete = (e: ClipEntry) =>
    clipboardDelete(e.id)
      .then(() => load(0, false))
      .catch(() => showToast("删除失败"));

  return (
    <>
      <div className={styles.list} ref={listRef}>
        {entries.length === 0 ? (
          <div className={styles.empty}>
            <div>
              <Text size={400} weight="semibold" block>
                没有匹配的记录
              </Text>
              <Text size={300} block style={{ marginTop: "8px" }}>
                复制任意内容后，这里会显示历史记录（文本实时捕获）。
              </Text>
            </div>
          </div>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((vi) => {
              const e = entries[vi.index];
              const isCode = e.group === "code" || e.group === "json";
              return (
                <div
                  key={e.id}
                  className={styles.entry}
                  style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${vi.start}px)` }}
                  onClick={() => doPaste(e)}
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
                      {e.source_app && <span className={styles.src}>{e.source_app}</span>}
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
                          doDelete(e);
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
              onClick={() => load(page + 1, true)}
            >
              加载更多
            </button>
          </div>
        )}
      </div>
      {toast && (
        <div className={styles.toast}>
          <Badge appearance="filled" color="success">
            ✓
          </Badge>{" "}
          {toast}
        </div>
      )}
    </>
  );
}

// 保持 IN_TAURI 引用（浏览器预览时禁用事件订阅）
export const _IN_TAURI = IN_TAURI;
