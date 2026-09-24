import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Dropdown,
  Option,
} from "@fluentui/react-components";
import { marked } from "marked";
import {
  notesBacklinks,
  notesByTag,
  notesCanvasDirs,
  notesCanvasGet,
  notesCanvasSave,
  notesCardCreate,
  notesCardDelete,
  notesCards,
  notesCreate,
  notesDelete,
  notesLinks,
  notesList,
  notesRead,
  notesRename,
  notesReindex,
  notesReviewGrade,
  notesReviewQueue,
  notesSearch,
  notesSync,
  notesWrite,
  parseAppError,
  type CanvasDocDto,
  type CanvasNodeDto,
  type NoteBacklinkDto,
  type NoteCardDto,
  type NoteLinkDto,
  type NoteMetaDto,
  type NoteSearchHitDto,
  type NoteSyncResultDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { keyActivate } from "../../a11y";
import Section from "../../components/Section";
import Tabs from "../../components/Tabs";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 笔记与知识面板（docs/impl/06 N1–N5，M10 v1）：
 * - N1 笔记库：磁盘 .md 为真相源；列表/编辑/预览/标签，保存后索引即时更新
 * - N2 双链：[[目标|别名]] 出链与反链面板；重命名全库引用改写
 * - N3 画布：目录级 .nforge-canvas.json，节点拖拽 + 便签/笔记引用 + 有向连线
 * - N4 复习：SM-2 简化版四档评分（忘记1/困难3/良好4/简单5），到期队列
 * - sync：外部编辑器改动增量收敛（进入面板与 notes.changed 事件触发）；
 *   「手动同步」按钮与自动 effect 共用同一在途 promise 去重（T-B1-10）
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
  grow: { flex: 1, minWidth: "160px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  split: { display: "grid", gridTemplateColumns: "280px 1fr 170px", gap: "12px", alignItems: "start" },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
    maxHeight: "480px",
    overflowY: "auto",
  },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    cursor: "pointer",
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  itemActive: { backgroundColor: tokens.colorNeutralBackground3Hover },
  outline: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
    maxHeight: "480px",
    overflowY: "auto",
    borderLeft: `1px solid ${tokens.colorNeutralStroke1}`,
    paddingLeft: "8px",
    minWidth: 0,
  },
  outlineItem: {
    cursor: "pointer",
    padding: "2px 4px",
    borderRadius: tokens.borderRadiusSmall,
    whiteSpace: "nowrap",
    textOverflow: "ellipsis",
    overflow: "hidden",
  },
  editorArea: {
    width: "100%",
    minHeight: "380px",
    resize: "vertical",
    padding: "10px 12px",
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase300,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  preview: {
    minHeight: "380px",
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "12px 16px",
    backgroundColor: tokens.colorNeutralBackground2,
  },
  canvasWrap: {
    position: "relative",
    height: "460px",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground2,
    overflow: "hidden",
  },
  node: {
    position: "absolute",
    padding: "8px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground1,
    cursor: "grab",
    fontSize: tokens.fontSizeBase200,
    overflow: "hidden",
  },
  nodeSelected: { border: `2px solid ${tokens.colorBrandForeground1}` },
  nodeSticky: { backgroundColor: tokens.colorPaletteMarigoldBackground1 },
  card: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "12px 16px",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    minHeight: "160px",
  },
  gradeRow: { display: "flex", gap: "8px", flexWrap: "wrap" },
});

type TabId = "notes" | "review" | "canvas";
const GRADES: { q: number; label: string }[] = [
  { q: 1, label: "忘记" },
  { q: 3, label: "困难" },
  { q: 4, label: "良好" },
  { q: 5, label: "简单" },
];

/** 大纲条目（T-B7-22 前端纯函数产物；line 为 1 基） */
export type NoteHeading = { level: number; text: string; line: number };

/**
 * 提取大纲（任务书：不进后端——内容在 read 手里）：ATX + Setext 两式；
 * 代码围栏（``` / ~~~）内的 `#` 不算；frontmatter 块整体跳过。
 */
export function extractHeadings(md: string): NoteHeading[] {
  const lines = md.split(/\r?\n/);
  const out: NoteHeading[] = [];
  let fence: string | null = null;
  let i = 0;
  if (lines[0]?.trim() === "---") {
    for (let j = 1; j < lines.length; j++) {
      const t = lines[j].trim();
      if (t === "---" || t === "...") {
        i = j + 1;
        break;
      }
    }
  }
  for (; i < lines.length; i++) {
    const t = lines[i].trim();
    if (fence) {
      if (t.startsWith(fence)) fence = null;
      continue;
    }
    if (t.startsWith("```") || t.startsWith("~~~")) {
      fence = t.slice(0, 3);
      continue;
    }
    const atx = /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(t);
    if (atx) {
      out.push({ level: atx[1].length, text: atx[2].trim() || "(无题)", line: i + 1 });
      continue;
    }
    // Setext：独占一行的 === / --- 下划线，且上一行是非空段落行
    if (/^=+$/.test(t) || /^-{3,}$/.test(t)) {
      const prev = i > 0 ? lines[i - 1].trim() : "";
      if (prev && !/^(#{1,6}\s|>)/.test(prev)) {
        out.push({ level: t.startsWith("=") ? 1 : 2, text: prev, line: i });
      }
    }
  }
  return out;
}

export default function NotesPanel() {
  const styles = useStyles();
  const [tab, setTab] = useState<TabId>("notes");

  // ---- 笔记列表 ----
  const [all, setAll] = useState<NoteMetaDto[]>([]);
  const [filter, setFilter] = useState("");
  // T-B7-21：搜索走后端 FTS5（null=未在搜索态）
  const [hits, setHits] = useState<NoteSearchHitDto[] | null>(null);
  // T-B7-22：标签过滤走后端精确查询（tagList=null 即未过滤）
  const [activeTag, setActiveTag] = useState<string | null>(null);
  const [tagList, setTagList] = useState<NoteMetaDto[] | null>(null);
  const [active, setActive] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [dirty, setDirty] = useState(false);
  const [preview, setPreview] = useState(false);
  const [newName, setNewName] = useState("");
  const [links, setLinks] = useState<NoteLinkDto[]>([]);
  const [backlinks, setBacklinks] = useState<NoteBacklinkDto[]>([]);
  const [msg, setMsg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // ---- 复习 ----
  const [queue, setQueue] = useState<NoteCardDto[]>([]);
  const [allCards, setAllCards] = useState<NoteCardDto[]>([]);
  const [revealed, setRevealed] = useState(false);
  const [cardFront, setCardFront] = useState("");
  const [cardBack, setCardBack] = useState("");

  // ---- 画布 ----
  const [dirs, setDirs] = useState<string[]>([]);
  const [canvasDir, setCanvasDir] = useState("");
  const [doc, setDoc] = useState<CanvasDocDto>({ version: 1, nodes: [], edges: [] });
  const [selNode, setSelNode] = useState<string | null>(null);
  const linkMode = useRef<string | null>(null); // 连线模式：第一个端点
  const editorRef = useRef<HTMLTextAreaElement | null>(null);
  const dragRef = useRef<{ id: string; dx: number; dy: number } | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  const fail = useCallback((e: unknown) => {
    setErr(parseAppError(e)?.data.message ?? String(e));
  }, []);

  const refreshList = useCallback(async () => {
    try {
      const list = await notesList();
      setAll(list);
    } catch (e) {
      reportError(e, { context: "笔记列表加载失败", dedupeKey: "notes-list", toast: false });
    } finally {
      setLoading(false);
    }
  }, []);

  // T-B7-21：搜索词防抖后走后端全文索引（200ms；空串退出搜索态）
  useEffect(() => {
    const q = filter.trim();
    if (!q) {
      setHits(null);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      void notesSearch(q, 200)
        .then((r) => {
          if (!cancelled) setHits(r);
        })
        .catch((e) => {
          if (!cancelled) fail(e);
        });
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [filter, fail]);

  // T-B7-22：标签芯片点选=后端精确查询；再点同一芯片退出
  const toggleTag = useCallback(
    (t: string) => {
      if (activeTag === t) {
        setActiveTag(null);
        setTagList(null);
        return;
      }
      setActiveTag(t);
      void notesByTag(t)
        .then(setTagList)
        .catch(fail);
    },
    [activeTag, fail],
  );

  const refreshReview = useCallback(async () => {
    try {
      const [q, a] = await Promise.all([notesReviewQueue(), notesCards()]);
      setQueue(q);
      setAllCards(a);
    } catch (e) {
      reportError(e, { context: "复习队列加载失败", dedupeKey: "notes-review", toast: false });
    }
  }, []);

  const openNote = useCallback(
    async (path: string) => {
      setErr(null);
      try {
        const r = await notesRead(path);
        setActive(path);
        setContent(r.content);
        setDirty(false);
        setLinks(await notesLinks(path));
        setBacklinks(await notesBacklinks(path));
      } catch (e) {
        fail(e);
      }
    },
    [fail],
  );

  // 首次加载 + notes.changed 事件驱动刷新
  useEffect(() => {
    void refreshList();
    void refreshReview();
    void notesCanvasDirs()
      .then(setDirs)
      .catch((e) =>
        reportError(e, { context: "画板目录加载失败", dedupeKey: "notes-canvas-dirs", toast: false }),
      );
    if (!("__TAURI_INTERNALS__" in window)) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<{ topic: string }>("nf:event", (e) => {
          if (e.payload?.topic === "notes.changed") {
            void refreshList();
            void refreshReview();
          }
        }),
      )
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refreshList, refreshReview]);

  // 进入面板时增量收敛外部编辑器的改动
  // 手动同步（T-B1-10）与自动 effect 共用同一在途 promise（syncInFlight）：
  // busy 期（含连点、含与进面板 effect 竞速）都复用同一次 notes_sync invoke
  const syncInFlight = useRef<Promise<NoteSyncResultDto> | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const startSync = useCallback(() => {
    if (syncInFlight.current) return syncInFlight.current;
    setSyncBusy(true);
    const p = notesSync().finally(() => {
      syncInFlight.current = null;
      setSyncBusy(false);
    });
    syncInFlight.current = p;
    return p;
  }, []);
  useEffect(() => {
    void startSync()
      .then((r) => {
        if (r.updated + r.added + r.removed > 0) void refreshList();
      })
      .catch((e) => reportError(e, { context: "笔记索引增量同步失败", dedupeKey: "notes-sync" }));
  }, [startSync, refreshList]);

  const doManualSync = useCallback(async () => {
    try {
      const r = await startSync();
      const changed = r.added + r.updated + r.removed;
      setMsg(
        changed > 0
          ? `同步完成：新增 ${r.added} · 更新 ${r.updated} · 移除 ${r.removed}`
          : `同步完成：无外部改动（索引 ${r.total} 篇）`,
      );
      if (changed > 0) await refreshList();
    } catch (e) {
      fail(e);
    }
  }, [startSync, refreshList, fail]);

  const saveNote = useCallback(async () => {
    if (!active) return;
    setErr(null);
    try {
      await notesWrite(active, content);
      setDirty(false);
      setMsg(`已保存 ${active}`);
      setLinks(await notesLinks(active));
      setBacklinks(await notesBacklinks(active));
    } catch (e) {
      fail(e);
    }
  }, [active, content, fail]);

  // Ctrl+S 保存
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s" && tab === "notes") {
        e.preventDefault();
        void saveNote();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [saveNote, tab]);

  const createNote = useCallback(async () => {
    const name = newName.trim();
    if (!name) return;
    const rel = name.endsWith(".md") ? name : `${name}.md`;
    setErr(null);
    try {
      const meta = await notesCreate(rel);
      setNewName("");
      await refreshList();
      await openNote(meta.path);
    } catch (e) {
      fail(e);
    }
  }, [newName, refreshList, openNote, fail]);

  const deleteNote = useCallback(async () => {
    if (!active) return;
    setErr(null);
    if (
      !(await confirmAction({
        title: "删除笔记",
        impact: [`将删除笔记「${active}」`],
        detail:
          backlinks.length > 0
            ? `磁盘 .md 文件将被删除且不可恢复；有 ${backlinks.length} 条反链指向它，删除后这些 [[链接]] 将悬空。`
            : "磁盘 .md 文件将被删除且不可恢复。",
        confirmLabel: "删除",
      }))
    )
      return;
    try {
      await notesDelete(active);
      setActive(null);
      setContent("");
      setMsg(`已删除 ${active}`);
      await refreshList();
    } catch (e) {
      fail(e);
    }
  }, [active, backlinks.length, refreshList, fail]);

  const renameNote = useCallback(async () => {
    if (!active || !newName.trim()) return;
    const dir = active.includes("/") ? active.slice(0, active.lastIndexOf("/") + 1) : "";
    const name = newName.trim();
    const target = `${dir}${name.endsWith(".md") ? name : `${name}.md`}`;
    setErr(null);
    if (
      !(await confirmAction({
        title: "重命名笔记",
        impact: [`「${active}」 → 「${target}」`],
        detail: `全库 [[双链]] 引用将同步改写（当前 ${links.length} 出链 / ${backlinks.length} 反链受影响）。`,
        danger: false,
        confirmLabel: "重命名",
      }))
    )
      return;
    try {
      await notesRename(active, target);
      setNewName("");
      setMsg(`已重命名 → ${target}（全库引用已同步）`);
      await refreshList();
      await openNote(target);
    } catch (e) {
      fail(e);
    }
  }, [active, newName, links.length, backlinks.length, refreshList, openNote, fail]);

  // ---- 复习动作 ----
  const grade = useCallback(
    async (q: number) => {
      const head = queue[0];
      if (!head) return;
      setRevealed(false);
      try {
        await notesReviewGrade(head.id, q);
        setQueue((prev) => prev.slice(1));
        await refreshReview();
      } catch (e) {
        fail(e);
      }
    },
    [queue, refreshReview, fail],
  );

  const addCard = useCallback(async () => {
    if (!cardFront.trim()) return;
    setErr(null);
    try {
      await notesCardCreate(cardFront, cardBack, active ?? undefined);
      setCardFront("");
      setCardBack("");
      setMsg("卡片已入队");
      await refreshReview();
    } catch (e) {
      fail(e);
    }
  }, [cardFront, cardBack, active, refreshReview, fail]);

  const deleteCard = useCallback(
    async (c: NoteCardDto) => {
      if (
        !(await confirmAction({
          title: "删除复习卡片",
          impact: [`「${c.front}」将从卡片库删除`],
          detail: "复习进度（间隔/E/F）一并丢失，不可恢复。",
          confirmLabel: "删除",
        }))
      )
        return;
      try {
        await notesCardDelete(c.id);
        await refreshReview();
      } catch (e) {
        fail(e);
      }
    },
    [refreshReview, fail],
  );

  // ---- 画布动作 ----
  const loadCanvas = useCallback(
    async (dir: string) => {
      setCanvasDir(dir);
      setErr(null);
      try {
        setDoc(await notesCanvasGet(dir));
        setSelNode(null);
      } catch (e) {
        fail(e);
      }
    },
    [fail],
  );

  const saveCanvas = useCallback(
    async (next: CanvasDocDto) => {
      setDoc(next);
      try {
        await notesCanvasSave(canvasDir, next);
        setMsg(`画布已保存（${next.nodes.length} 节点 / ${next.edges.length} 连线）`);
      } catch (e) {
        fail(e);
      }
    },
    [canvasDir, fail],
  );

  const addSticky = useCallback(() => {
    const n: CanvasNodeDto = {
      id: `n${Date.now().toString(36)}`,
      kind: "sticky",
      x: 40 + Math.random() * 200,
      y: 40 + Math.random() * 160,
      w: 180,
      h: 90,
      text: "新便签",
    };
    void saveCanvas({ ...doc, nodes: [...doc.nodes, n] });
  }, [doc, saveCanvas]);

  const [refPath, setRefPath] = useState<string | null>(null);
  const addRef = useCallback(() => {
    if (!refPath) return;
    const n: CanvasNodeDto = {
      id: `n${Date.now().toString(36)}`,
      kind: "note",
      x: 40 + Math.random() * 200,
      y: 40 + Math.random() * 160,
      w: 200,
      h: 70,
      ref: refPath,
    };
    void saveCanvas({ ...doc, nodes: [...doc.nodes, n] });
  }, [doc, refPath, saveCanvas]);

  const onNodePointerDown = useCallback(
    (e: React.PointerEvent, n: CanvasNodeDto) => {
      e.stopPropagation();
      if (linkMode.current && linkMode.current !== n.id) {
        const from = linkMode.current;
        linkMode.current = null;
        void saveCanvas({
          ...doc,
          edges: [...doc.edges, { id: `e${Date.now().toString(36)}`, from, to: n.id }],
        });
        return;
      }
      setSelNode(n.id);
      const rect = wrapRef.current?.getBoundingClientRect();
      dragRef.current = {
        id: n.id,
        dx: e.clientX - (rect?.left ?? 0) - n.x,
        dy: e.clientY - (rect?.top ?? 0) - n.y,
      };
    },
    [doc, saveCanvas],
  );

  const onWrapPointerMove = useCallback(
    (e: React.PointerEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const rect = wrapRef.current?.getBoundingClientRect();
      const nx = e.clientX - (rect?.left ?? 0) - d.dx;
      const ny = e.clientY - (rect?.top ?? 0) - d.dy;
      setDoc((prev) => ({
        ...prev,
        nodes: prev.nodes.map((n) => (n.id === d.id ? { ...n, x: Math.max(0, nx), y: Math.max(0, ny) } : n)),
      }));
    },
    [],
  );

  const onWrapPointerUp = useCallback(() => {
    if (dragRef.current) {
      dragRef.current = null;
      void notesCanvasSave(canvasDir, doc).catch((e) =>
        reportError(e, { context: "画板保存失败", dedupeKey: "notes-canvas-save" }),
      );
    }
  }, [canvasDir, doc]);

  const removeSelected = useCallback(async () => {
    if (!selNode) return;
    const node = doc.nodes.find((n) => n.id === selNode);
    const edgeCount = doc.edges.filter((e) => e.from === selNode || e.to === selNode).length;
    if (
      !(await confirmAction({
        title: "删除画布节点",
        impact: [
          `将删除节点「${node?.text ?? node?.ref ?? selNode}」`,
          `${edgeCount} 条关联连线将一并删除`,
        ],
        detail: "点击「保存画布」前不会写盘，但本视图内不撤销。",
        danger: false,
        confirmLabel: "删除",
      }))
    )
      return;
    void saveCanvas({
      ...doc,
      nodes: doc.nodes.filter((n) => n.id !== selNode),
      edges: doc.edges.filter((e) => e.from !== selNode && e.to !== selNode),
    });
    setSelNode(null);
  }, [selNode, doc, saveCanvas]);

  // T-B7-21：搜索走后端后按 title/path/body 三列分组（先命中列归属；组内保持 rank 序）；
  // 标签列仍内存补搜（tags 不进 fts，旧标签搜索语义不丢）
  const q = filter.trim().toLowerCase();
  const searching = q.length > 0;
  const hitList = hits ?? [];
  const hitPaths = new Set(hitList.map((h) => h.path));
  const groupOf = (h: NoteSearchHitDto) =>
    h.title.toLowerCase().includes(q) ? "标题" : h.path.toLowerCase().includes(q) ? "路径" : "正文";
  const groups = (["标题", "路径", "正文"] as const).map((g) => ({
    g,
    rows: hitList.filter((h) => groupOf(h) === g),
  }));
  const tagExtra = searching
    ? all.filter((n) => !hitPaths.has(n.path) && n.tags.some((t) => t.toLowerCase().includes(q)))
    : [];
  // T-B7-22：标签芯片行（全库标签并集）+ 过滤态基础列表
  const allTags = useMemo(
    () => Array.from(new Set(all.flatMap((n) => n.tags))).sort((a, b) => a.localeCompare(b, "zh")),
    [all],
  );
  const baseList = tagList ?? all;
  // T-B7-22：大纲（当前笔记正文的纯函数投影）
  const headings = useMemo(() => (active ? extractHeadings(content) : []), [active, content]);
  const jumpToLine = (line: number) => {
    if (preview) {
      setPreview(false); // 预览态先回编辑态；再次点击即定位
      return;
    }
    const ta = editorRef.current;
    if (!ta) return;
    const pos = content.split("\n").slice(0, line - 1).join("\n").length + (line > 1 ? 1 : 0);
    ta.focus();
    ta.setSelectionRange(pos, pos);
    ta.scrollTop = Math.max(0, (line - 3) * 20);
  };

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <Tabs
          ariaLabel="笔记视图"
          value={tab}
          onChange={setTab}
          items={[
            { id: "notes", label: `笔记（${all.length}）` },
            { id: "review", label: `复习（${queue.length} 到期）` },
            { id: "canvas", label: "画布" },
          ]}
        />
        <div className={styles.grow} />
        <Button
          size="small"
          onClick={() =>
            void notesReindex()
              .then((r) => {
                setMsg(`索引重建：${r.total} 篇`);
                return refreshList();
              })
              .catch(fail)
          }
        >
          重建索引
        </Button>
      </div>

      <InlineError text={msg} tone="success" />
      <InlineError text={err} />

      {tab === "notes" && (
        <Section>
          <div className={styles.row}>
            <Input
              className={styles.grow}
              placeholder="搜索标题/路径/标签/正文（后端全文索引）"
              value={filter}
              onChange={(_, d) => setFilter(d.value)}
            />
            <Input
              placeholder="新笔记名或 sub/名称.md"
              value={newName}
              onChange={(_, d) => setNewName(d.value)}
            />
            <Button appearance="primary" size="small" onClick={() => void createNote()}>
              新建
            </Button>
            {active && (
              <>
                <Button size="small" onClick={() => void renameNote()}>
                  重命名为输入值
                </Button>
                <Button size="small" appearance="subtle" onClick={() => void deleteNote()}>
                  删除
                </Button>
              </>
            )}
            <Button size="small" disabled={syncBusy} onClick={() => void doManualSync()}>
              {syncBusy ? "同步中…" : "手动同步"}
            </Button>
          </div>
          {allTags.length > 0 && (
            <div className={styles.row} style={{ marginTop: 8 }}>
              {allTags.map((t) => (
                <Button
                  key={t}
                  size="small"
                  appearance={activeTag === t ? "primary" : "subtle"}
                  aria-pressed={activeTag === t}
                  onClick={() => toggleTag(t)}
                >
                  #{t}
                </Button>
              ))}
              {activeTag && (
                <Text size={100} className={styles.muted}>
                  标签「{activeTag}」{tagList ? `：${tagList.length} 篇（后端精确查询）` : "：查询中…"}
                </Text>
              )}
            </div>
          )}
          <div className={styles.split}>
            <div className={styles.list}>
              {!searching &&
                baseList.map((n) => (
                  <div
                    key={n.path}
                    className={`${styles.item} ${active === n.path ? styles.itemActive : ""}`}
                    onClick={() => void openNote(n.path)}
                    role="button"
                    tabIndex={0}
                    onKeyDown={keyActivate(() => void openNote(n.path))}
                  >
                    <Text size={300} weight="semibold">
                      {n.title}
                      {n.tags.slice(0, 3).map((t) => (
                        <Badge key={t} size="small" appearance="outline" style={{ marginLeft: 6 }}>
                          {t}
                        </Badge>
                      ))}
                    </Text>
                    <Text size={100} className={styles.muted}>
                      {n.path} · {new Date(n.mtime_ms).toLocaleString()}
                    </Text>
                  </div>
                ))}
              {searching &&
                groups.map(({ g, rows }) =>
                  rows.length === 0 ? null : (
                    <div key={g}>
                      <Text size={100} className={styles.muted}>
                        {g}命中（{rows.length}）
                      </Text>
                      {rows.map((h) => (
                        <div
                          key={h.path}
                          className={`${styles.item} ${active === h.path ? styles.itemActive : ""}`}
                          onClick={() => void openNote(h.path)}
                          role="button"
                          tabIndex={0}
                          onKeyDown={keyActivate(() => void openNote(h.path))}
                        >
                          <Text size={300} weight="semibold">
                            {h.title || h.path}
                          </Text>
                          <Text size={100} className={styles.muted}>
                            {h.path}
                            {g === "正文" ? ` · ${h.snippet}` : ""}
                          </Text>
                        </div>
                      ))}
                    </div>
                  ),
                )}
              {searching && tagExtra.length > 0 && (
                <div>
                  <Text size={100} className={styles.muted}>
                    标签命中（{tagExtra.length}）
                  </Text>
                  {tagExtra.map((n) => (
                    <div
                      key={n.path}
                      className={`${styles.item} ${active === n.path ? styles.itemActive : ""}`}
                      onClick={() => void openNote(n.path)}
                      role="button"
                      tabIndex={0}
                      onKeyDown={keyActivate(() => void openNote(n.path))}
                    >
                      <Text size={300} weight="semibold">
                        {n.title}
                      </Text>
                      <Text size={100} className={styles.muted}>
                        {n.path}
                      </Text>
                    </div>
                  ))}
                </div>
              )}
              {(loading ||
                (!searching && baseList.length === 0) ||
                (searching && hitList.length === 0 && tagExtra.length === 0)) && (
                <EmptyState
                  text={
                    searching
                      ? "无匹配：搜索已走后端全文索引（标题/路径/正文/标签）"
                      : activeTag
                        ? `标签「${activeTag}」下暂无笔记`
                        : "暂无笔记：在上方输入名称新建，或点「重建索引」扫描磁盘 .md"
                  }
                  loading={loading}
                />
              )}
            </div>
            <div style={{ display: "flex", flexDirection: "column", gap: 8, minWidth: 0 }}>
              {active ? (
                <>
                  <div className={styles.row}>
                    <Text size={300} weight="semibold">
                      {active}
                    </Text>
                    {dirty && <Badge appearance="filled" color="warning">未保存</Badge>}
                    <div className={styles.grow} />
                    <Button size="small" onClick={() => setPreview((p) => !p)}>
                      {preview ? "编辑" : "预览"}
                    </Button>
                    <Button size="small" appearance="primary" onClick={() => void saveNote()}>
                      保存（Ctrl+S）
                    </Button>
                  </div>
                  {preview ? (
                    <div
                      className={styles.preview}
                      dangerouslySetInnerHTML={{ __html: marked.parse(content) as string }}
                    />
                  ) : (
                    <textarea
                      className={styles.editorArea}
                      ref={editorRef}
                      value={content}
                      onChange={(e) => {
                        setContent(e.target.value);
                        setDirty(true);
                      }}
                    />
                  )}
                  <div className={styles.row}>
                    <Text size={200} weight="semibold">
                      出链：{links.length}
                    </Text>
                    {links.map((l) => (
                      <Badge
                        key={l.dst}
                        appearance="outline"
                        style={{ cursor: l.dst_path ? "pointer" : "default" }}
                        onClick={() => l.dst_path && void openNote(l.dst_path)}
                      >
                        {l.dst}
                        {!l.dst_path && "（未解析）"}
                      </Badge>
                    ))}
                  </div>
                  <div className={styles.row}>
                    <Text size={200} weight="semibold">
                      反链：{backlinks.length}
                    </Text>
                    {backlinks.map((b) => (
                      <Badge
                        key={b.src}
                        appearance="outline"
                        title={b.snippet}
                        style={{ cursor: "pointer" }}
                        onClick={() => void openNote(b.src)}
                      >
                        {b.title}
                      </Badge>
                    ))}
                  </div>
                </>
              ) : (
                <Text className={styles.muted}>选择左侧笔记，或新建一篇（[[双链]] 语法可在任意笔记中引用其他笔记）</Text>
              )}
            </div>
            <div className={styles.outline}>
              <Text size={200} weight="semibold">
                大纲
              </Text>
              {headings.map((h) => (
                <div
                  key={`${h.line}-${h.text}`}
                  role="button"
                  tabIndex={0}
                  className={styles.outlineItem}
                  style={{ paddingLeft: 8 + (h.level - 1) * 10 }}
                  title={h.text}
                  onClick={() => jumpToLine(h.line)}
                  onKeyDown={keyActivate(() => jumpToLine(h.line))}
                >
                  <Text size={200}>
                    {h.text}
                  </Text>
                </div>
              ))}
              {active && headings.length === 0 && (
                <Text size={100} className={styles.muted}>
                  无标题
                </Text>
              )}
            </div>
          </div>
        </Section>
      )}

      {tab === "review" && (
        <Section>
          <div className={styles.row}>
            <Input className={styles.grow} placeholder="卡片正面" value={cardFront} onChange={(_, d) => setCardFront(d.value)} />
            <Input className={styles.grow} placeholder="卡片背面（可空）" value={cardBack} onChange={(_, d) => setCardBack(d.value)} />
            <Button appearance="primary" size="small" onClick={() => void addCard()}>
              新建卡片{active ? `（关联 ${active}）` : ""}
            </Button>
          </div>
          {queue.length > 0 ? (
            <div className={styles.card}>
              <Text size={400} weight="semibold">
                {queue[0].front}
              </Text>
              {revealed ? (
                <>
                  <Text size={300}>{queue[0].back || "（无背面内容）"}</Text>
                  <div className={styles.gradeRow}>
                    {GRADES.map((g) => (
                      <Button key={g.q} size="small" onClick={() => void grade(g.q)}>
                        {g.label}
                      </Button>
                    ))}
                  </div>
                </>
              ) : (
                <Button size="small" onClick={() => setRevealed(true)}>
                  显示答案
                </Button>
              )}
              <Text className={styles.muted}>
                队列 {queue.length} 张 · SM-2 间隔重复（忘记→1 天后重来）
              </Text>
            </div>
          ) : (
            <Text className={styles.muted}>今天没有到期卡片</Text>
          )}
          <div className={styles.row}>
            <Text size={200} weight="semibold">
              全部卡片（{allCards.length}）
            </Text>
          </div>
          <div className={styles.list}>
            {allCards.map((c) => (
              <div key={c.id} className={`${styles.item} ${styles.row}`}>
                <Text size={200}>{c.front}</Text>
                <Text size={100} className={styles.muted}>
                  间隔 {c.interval_days} 天 · EF {c.ef.toFixed(2)} ·{" "}
                  {c.due_ms <= Date.now() ? "已到期" : new Date(c.due_ms).toLocaleDateString()}
                </Text>
                <div className={styles.grow} />
                <Button size="small" appearance="subtle" onClick={() => void deleteCard(c)}>
                  删除
                </Button>
              </div>
            ))}
          </div>
        </Section>
      )}

      {tab === "canvas" && (
        <Section>
          <div className={styles.row}>
            <Dropdown
              className={styles.grow}
              value={canvasDir === "" ? "（库根）" : canvasDir}
              selectedOptions={[canvasDir]}
              onOptionSelect={(_, d) => void loadCanvas(String(d.optionValue ?? ""))}
            >
              {dirs.map((d) => (
                <Option key={d} value={d} text={d === "" ? "（库根）" : d}>
                  {d === "" ? "（库根）" : d}
                </Option>
              ))}
            </Dropdown>
            <Button size="small" onClick={addSticky}>
              添加便签
            </Button>
            <Dropdown
              placeholder="引用笔记…"
              value={refPath ?? "引用笔记…"}
              selectedOptions={refPath ? [refPath] : []}
              onOptionSelect={(_, d) => setRefPath(String(d.optionValue ?? ""))}
            >
              {all.slice(0, 50).map((n) => (
                <Option key={n.path} value={n.path} text={n.path}>
                  {n.path}
                </Option>
              ))}
            </Dropdown>
            <Button size="small" onClick={addRef}>
              添加引用
            </Button>
            <Button
              size="small"
              onClick={() => {
                linkMode.current = selNode;
                setMsg(selNode ? "连线模式：点击目标节点" : "先选中起点节点再连线");
              }}
            >
              从选中节点连线
            </Button>
            <Button size="small" appearance="subtle" onClick={() => void removeSelected()}>
              删除选中
            </Button>
            <Button size="small" appearance="primary" onClick={() => void saveCanvas(doc)}>
              保存画布
            </Button>
          </div>
          <div
            ref={wrapRef}
            className={styles.canvasWrap}
            onPointerMove={onWrapPointerMove}
            onPointerUp={onWrapPointerUp}
            onPointerDown={() => setSelNode(null)}
          >
            <svg width="100%" height="100%" style={{ position: "absolute", inset: 0, pointerEvents: "none" }}>
              {doc.edges.map((e) => {
                const a = doc.nodes.find((n) => n.id === e.from);
                const b = doc.nodes.find((n) => n.id === e.to);
                if (!a || !b) return null;
                return (
                  <line
                    key={e.id}
                    x1={a.x + a.w / 2}
                    y1={a.y + a.h / 2}
                    x2={b.x + b.w / 2}
                    y2={b.y + b.h / 2}
                    stroke={tokens.colorBrandForeground1}
                    strokeWidth={1.5}
                    markerEnd=""
                  />
                );
              })}
            </svg>
            {doc.nodes.map((n) => (
              <div
                key={n.id}
                className={`${styles.node} ${selNode === n.id ? styles.nodeSelected : ""} ${n.kind === "sticky" ? styles.nodeSticky : ""}`}
                style={{ left: n.x, top: n.y, width: n.w, height: n.h }}
                onPointerDown={(e) => onNodePointerDown(e, n)}
              >
                {n.kind === "note" ? (
                  <Text size={200} weight="semibold">
                    📄 {n.ref}
                  </Text>
                ) : (
                  <Text size={200}>{n.text}</Text>
                )}
              </div>
            ))}
            {doc.nodes.length === 0 && (
              <Text className={styles.muted} style={{ position: "absolute", left: 16, top: 16 }}>
                空画布——添加便签或笔记引用节点，拖动布局，保存为目录内 .nforge-canvas.json
              </Text>
            )}
          </div>
        </Section>
      )}
    </div>
  );
}
