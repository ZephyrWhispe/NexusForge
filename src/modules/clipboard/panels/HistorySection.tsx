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
  clipboardHtmlGet,
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
import Tabs from "../../../components/Tabs";
import DeferredBadge from "../../../components/DeferredBadge";
import DibThumb from "../DibThumb";
import { keyActivate } from "../../../a11y";
import { GROUP_LABEL, fmtTime } from "../display";

/**
 * 历史子面板（T-B3-1 自 ClipboardPanel 原样搬入，逻辑一行不删）：
 * 数据 clipboard_search FTS/分页 + group:/type: 语法芯片（T-B3-6）；
 * 实时 nf:event clipboard.captured/deleted/cleared → 刷新；
 * 行操作（详情/粘贴/置顶/删除）+ 工具栏清空 + 两个 Dialog 形态均沿用 B1 判据；
 * T-B3-8：详情框「纯文本 / HTML 源」两 Tab（源文经 clipboard_html_get 显式取、
 * 以 `<pre>` 文本渲染），行操作把 [粘贴] 拆成 [粘贴为纯文本][粘贴带格式]
 * （后者仅 has_html 行渲染），RTF 保格式粘贴按 D-29 B3 收窄挂延后徽标。
 */
const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    minHeight: "40px",
    flexWrap: "wrap",
    padding: "6px 20px",
    flexShrink: 0,
  },
  chips: {
    display: "flex",
    alignItems: "center",
    gap: "4px",
    marginLeft: "auto",
    flexWrap: "wrap",
  },
  filterChip: {
    fontSize: tokens.fontSizeBase200,
    padding: "2px 9px",
    borderRadius: tokens.borderRadiusCircular,
    border: `1px solid ${tokens.colorBrandForeground1}`,
    color: tokens.colorBrandForeground1,
    backgroundColor: "transparent",
    cursor: "pointer",
  },
  syntaxHint: {
    display: "block",
    margin: "0 20px 6px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
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

/** 后端 `group:` / `type:` 语法的前端镜像（权威实现见 crates/clipboard-core/src/query.rs）：
 * 镜像服务两件事——把已生效的筛选显式化（类型芯片高亮、只读组芯片），以及在被用户接管/
 * 清掉时把对应语法 token 从下传文本里剥出去。未知 type 值一律留在字面文本里下传，
 * 前端不另判一套：零命中由后端 `AND 0` 给出，判定只有一份。 */
export interface ClipSyntaxMirror {
  /** 去掉已消费语法 token 后的字面查询（未知 type 的 token 算未消费，留在里面） */
  text: string;
  group?: string;
  groupToken?: string;
  type?: string;
  typeToken?: string;
  unknownType: boolean;
  /** 最后一枚未知 type token（留在 text 里下传，供诚实提示点名） */
  unknownToken?: string;
}

const TYPE_VALUES = ["text", "image", "files"];
const TYPE_CHIPS: [string, string][] = [
  ["text", "文本"],
  ["image", "图片"],
  ["files", "文件"],
];

export function mirrorClipSearchSyntax(raw: string): ClipSyntaxMirror {
  const literal: string[] = [];
  let group: string | undefined;
  let groupToken: string | undefined;
  let type: string | undefined;
  let typeToken: string | undefined;
  let unknownType = false;
  let unknownToken: string | undefined;
  for (const token of tokenizeSearch(raw)) {
    const sep = token.includes("://") ? -1 : token.indexOf(":");
    const key = sep > 0 ? token.slice(0, sep) : "";
    const rest = sep > 0 ? token.slice(sep + 1) : "";
    if (key === "group" && rest !== "") {
      const value = stripQuotes(rest);
      if (value) {
        group = value;
        groupToken = token;
      } else {
        literal.push(token);
      }
    } else if (key === "type" && rest !== "") {
      const value = stripQuotes(rest);
      if (TYPE_VALUES.includes(value)) {
        type = value;
        typeToken = token;
      } else {
        unknownType = true;
        unknownToken = token;
        literal.push(token);
      }
    } else {
      literal.push(token);
    }
  }
  return {
    text: literal.join(" "),
    group,
    groupToken,
    type,
    typeToken,
    unknownType,
    unknownToken,
  };
}

/** 重组下传查询文本：保留的语法 token 留在文本里（后端 AND 两个分组维度），不保留的剥掉 */
export function clipQueryText(
  m: ClipSyntaxMirror,
  keep: { group: boolean; type: boolean },
): string {
  return [m.text, keep.group ? m.groupToken : "", keep.type ? m.typeToken : ""]
    .filter(Boolean)
    .join(" ")
    .trim();
}

function tokenizeSearch(raw: string): string[] {
  const tokens: string[] = [];
  let cur = "";
  let quoted = false;
  for (const ch of raw) {
    if (ch === '"') {
      quoted = !quoted;
      cur += ch;
    } else if (!quoted && /\s/.test(ch)) {
      if (cur) tokens.push(cur);
      cur = "";
    } else {
      cur += ch;
    }
  }
  if (cur) tokens.push(cur);
  return tokens;
}

function stripQuotes(value: string): string {
  const bare =
    value.length >= 2 && value.startsWith('"') && value.endsWith('"')
      ? value.slice(1, -1)
      : value;
  return bare.trim();
}

/**
 * 查询参数构造（纯函数，Vitest 覆盖）：空搜索不参与过滤；"all" 分组不进 SQL；分页页码原样下传。
 * `group:` 语法值覆盖 SubNav 筛选维度（打字输入比侧栏选中更具体，且芯片上看得见），
 * `type:` 语法值让位于芯片（点芯片是更近的一次意图）——两条 precedence 都各有点得见的落点。
 */
export function clipSearchParams(
  search: string,
  group: string,
  page: number,
  size = 50,
  type?: string,
): ClipSearchQuery {
  const m = mirrorClipSearchSyntax(search);
  return {
    text: m.text || undefined,
    group: m.group ?? (group !== "all" ? group : undefined),
    page,
    size,
    content_type: type ?? m.type,
  };
}

interface Props {
  search: string;
  group: string;
  onCounts: (counts: Record<string, number>) => void;
}

/** 详情正文两视图（T-B3-8）：常量显式定型，免得 Tabs 泛型把 id 推成 string */
const DETAIL_TABS: { id: "plain" | "html"; label: string }[] = [
  { id: "plain", label: "纯文本" },
  { id: "html", label: "HTML 源" },
];

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
  // T-B3-8：html 首次切到 HTML 源 Tab 才拉（列表只带布尔，正文按显式口取）
  const [detailTab, setDetailTab] = useState<"plain" | "html">("plain");
  const [detail, setDetail] = useState<{
    entry: ClipEntry;
    text: string | null;
    failed: string | null;
    html: { text: string | null; failed: string | null } | null;
  } | null>(null);
  // T-B3-4 详情内的手工分组草稿（打开详情时以条目现值播种）
  const [groupDraft, setGroupDraft] = useState("");
  // T-B3-2 暂停捕获横幅：读运行态（与设置卡/托盘同源），只在真跳过过内容时出现
  const [capture, setCapture] = useState<ClipCaptureState | null>(null);
  // T-B3-6 类型芯片与「已清掉的 group: 语法」——后者按搜索原文记账，
  // 用户再改一个字就自然失效（新那句里的 group: 也许是另一回事，不静默延续旧清空）
  const [typeTouched, setTypeTouched] = useState(false);
  const [typeChip, setTypeChip] = useState<string | undefined>(undefined);
  const [dismissedGroupFor, setDismissedGroupFor] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const mirror = mirrorClipSearchSyntax(search);
  const groupDismissed = dismissedGroupFor === search && dismissedGroupFor !== null;
  const queryText = clipQueryText(mirror, { group: !groupDismissed, type: !typeTouched });
  const effectiveType = typeTouched ? typeChip : mirror.type;

  const load = useCallback(
    async (p: number, append: boolean) => {
      try {
        const res = await clipboardSearch(clipSearchParams(queryText, group, p, 50, typeChip));
        setEntries((prev) => (append ? [...prev, ...res.items] : res.items));
        setHasMore(res.has_more);
        setPage(p);
      } finally {
        setLoaded(true);
      }
    },
    [queryText, group, typeChip],
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
        // T-B3-9：备份导入成功后宿主广播同一 groups_changed（零新主题），
        // 历史页因此在设置子面板里点完导入即回可见列表，不必切页重挂。
        if (topic === "clipboard.groups_changed")
          load(0, false).catch((e) =>
            reportError(e, { context: "备份导入后刷新失败", dedupeKey: "clip-import-refresh", toast: false }),
          );
        if (
          topic === "clipboard.deleted" ||
          topic === "clipboard.cleared" ||
          topic === "clipboard.captured" ||
          topic === "clipboard.groups_changed"
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

  // T-B3-8：默认纯文本；带格式粘贴只在有 HTML 的行上出现（后端缺席即降级，前端不放空钮）
  const doPaste = (e: ClipEntry, format: "plain" | "html" = "plain") =>
    clipboardPaste(e.id, format)
      .then((r) =>
        r.degraded
          ? notify(
              "warn",
              "已按纯文本粘贴",
              "该条目没有 HTML 正文：写进剪贴板的是纯文本那份，不带格式",
            )
          : notify(
              "success",
              r.format_used === "html" ? "已写入剪贴板（含 HTML）" : "已写入剪贴板",
              "回写窗口 500ms 内不重复记录",
            ),
      )
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
    setDetail({ entry: e, text: null, failed: null, html: null });
    setDetailTab("plain");
    setGroupDraft(e.group ?? "");
    // 图片走 DibThumb；敏感条目走通用读口必被 CLIPBOARD_GET_001 拒（T-B3-5 揭示门），
    // 详情框只显掩码并指路敏感库——把拒答当"读取失败"摆给用户看是第二种噪声。
    if (e.content_type === "image" || e.secret) return;
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

  /**
   * T-B3-8 切详情 Tab：HTML 源首次点开才经显式读口拉一次（列表不带正文），
   * 拉过就 cached——同一条源文重复来回看不重复打 IPC。
   */
  const switchDetailTab = (tab: "plain" | "html") => {
    setDetailTab(tab);
    if (tab !== "html") return;
    const entry = detail?.entry;
    if (!entry || detail?.html) return;
    setDetail((d) => (d ? { ...d, html: { text: null, failed: null } } : d));
    clipboardHtmlGet(entry.id)
      .then((t) =>
        setDetail((d) => (d && d.entry.id === entry.id ? { ...d, html: { text: t, failed: null } } : d)),
      )
      .catch((err) =>
        setDetail((d) =>
          d && d.entry.id === entry.id
            ? { ...d, html: { text: null, failed: parseAppError(err)?.data.message ?? String(err) } }
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
        <span className={styles.chips}>
          {TYPE_CHIPS.map(([value, label]) => (
            <Button
              key={value}
              size="small"
              appearance={effectiveType === value ? "primary" : "subtle"}
              onClick={() => {
                setTypeTouched(true);
                setTypeChip(effectiveType === value ? undefined : value);
              }}
            >
              {label}
            </Button>
          ))}
          {mirror.group && !groupDismissed && (
            <button
              className={styles.filterChip}
              aria-label="清除分组筛选"
              title="来自搜索语法 group: 的分组筛选，点一下清掉"
              onClick={() => setDismissedGroupFor(search)}
            >
              组：{mirror.group} ✕
            </button>
          )}
        </span>
      </div>
      {mirror.unknownType && (
        <span className={styles.syntaxHint}>
          未识别的内容类型「{(mirror.unknownToken ?? "").replace(/^type:/, "")}」不在 text / image /
          files 三档内：该条件按零命中处理，不会被静默忽略。
        </span>
      )}
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
                  <Tooltip content="粘贴为纯文本" relationship="label">
                    <button
                      className={styles.opBtn}
                      onClick={(ev) => {
                        ev.stopPropagation();
                        doPaste(e, "plain");
                      }}
                      aria-label="粘贴为纯文本"
                    >
                      ⏎
                    </button>
                  </Tooltip>
                  {/* T-B3-8：只有真带 HTML 正文的行才给带格式钮——点了才降级是诚实，
                      预先摆一个注定降级的钮是噪声 */}
                  {e.has_html && (
                    <Tooltip content="粘贴带格式（HTML）" relationship="label">
                      <button
                        className={styles.opBtn}
                        onClick={(ev) => {
                          ev.stopPropagation();
                          doPaste(e, "html");
                        }}
                        aria-label="粘贴带格式"
                      >
                        ◈
                      </button>
                    </Tooltip>
                  )}
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
                  {detail.entry.has_html && detail.entry.content_type === "text" && (
                    <div
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: "8px",
                        flexWrap: "wrap",
                        marginBottom: "6px",
                      }}
                    >
                      <Tabs
                        items={DETAIL_TABS}
                        value={detailTab}
                        onChange={switchDetailTab}
                        ariaLabel="详情正文格式"
                      />
                      {/* RTF 轨按 09 §8.2 T-B3-8 收窄：本版只做 HTML Format，保格式粘贴的
                          RTF 侧留徽标指路，不摆注定失败的禁用钮 */}
                      <DeferredBadge label="RTF 保格式粘贴" decisionRef="D-29 B3 收窄登记" />
                    </div>
                  )}
                  {detailTab === "html" && detail.entry.has_html ? (
                    detail.html && detail.html.failed ? (
                      <span className={styles.detailHint}>读取失败：{detail.html.failed}</span>
                    ) : detail.html && detail.html.text !== null ? (
                      /* 源文按文本渲染（pre + 文本子节点）：HTML 片段进 innerHTML 就是自造 XSS 面 */
                      <pre className={styles.fullText}>{detail.html.text}</pre>
                    ) : (
                      <span className={styles.detailHint}>加载中…</span>
                    )
                  ) : detail.entry.content_type === "image" ? (
                    <DibThumb id={detail.entry.id} width={420} height={280} />
                  ) : detail.entry.secret ? (
                    <>
                      <pre className={styles.fullText}>{detail.entry.preview}</pre>
                      <span className={styles.detailHint}>
                        敏感条目不在历史区揭示：请到「敏感库」视图逐行确认揭示（需二次确认，并写宿主审计日志）。
                      </span>
                    </>
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
