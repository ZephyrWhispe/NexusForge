import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Spinner,
  Dropdown,
  Option,
} from "@fluentui/react-components";
import { marked } from "marked";
import {
  notesBacklinks,
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
  notesSync,
  notesWrite,
  parseAppError,
  type CanvasDocDto,
  type CanvasNodeDto,
  type NoteBacklinkDto,
  type NoteCardDto,
  type NoteLinkDto,
  type NoteMetaDto,
} from "../../ipc/client";

/**
 * 笔记与知识面板（docs/impl/06 N1–N5，M10 v1）：
 * - N1 笔记库：磁盘 .md 为真相源；列表/编辑/预览/标签，保存后索引即时更新
 * - N2 双链：[[目标|别名]] 出链与反链面板；重命名全库引用改写
 * - N3 画布：目录级 .nforge-canvas.json，节点拖拽 + 便签/笔记引用 + 有向连线
 * - N4 复习：SM-2 简化版四档评分（忘记1/困难3/良好4/简单5），到期队列
 * - sync：外部编辑器改动增量收敛（进入面板与 notes.changed 事件触发）
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
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  grow: { flex: 1, minWidth: "160px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  ok: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
  tab: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "4px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    cursor: "pointer",
    fontSize: tokens.fontSizeBase200,
  },
  tabActive: {
    backgroundColor: tokens.colorNeutralBackground3Hover,
    border: `1px solid ${tokens.colorBrandForeground1}`,
  },
  split: { display: "grid", gridTemplateColumns: "300px 1fr", gap: "12px", alignItems: "start" },
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

export default function NotesPanel() {
  const styles = useStyles();
  const [tab, setTab] = useState<TabId>("notes");

  // ---- 笔记列表 ----
  const [all, setAll] = useState<NoteMetaDto[]>([]);
  const [filter, setFilter] = useState("");
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
  const dragRef = useRef<{ id: string; dx: number; dy: number } | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  const fail = useCallback((e: unknown) => {
    setErr(parseAppError(e)?.data.message ?? String(e));
  }, []);

  const refreshList = useCallback(async () => {
    try {
      const list = await notesList();
      setAll(list);
    } catch {
      /* 模块未就绪静默 */
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshReview = useCallback(async () => {
    try {
      const [q, a] = await Promise.all([notesReviewQueue(), notesCards()]);
      setQueue(q);
      setAllCards(a);
    } catch {
      /* 静默 */
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
    void notesCanvasDirs().then(setDirs).catch(() => {});
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
  useEffect(() => {
    void notesSync()
      .then((r) => {
        if (r.updated + r.added + r.removed > 0) void refreshList();
      })
      .catch(() => {});
  }, [refreshList]);

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
    try {
      await notesDelete(active);
      setActive(null);
      setContent("");
      setMsg(`已删除 ${active}`);
      await refreshList();
    } catch (e) {
      fail(e);
    }
  }, [active, refreshList, fail]);

  const renameNote = useCallback(async () => {
    if (!active || !newName.trim()) return;
    const dir = active.includes("/") ? active.slice(0, active.lastIndexOf("/") + 1) : "";
    const name = newName.trim();
    const target = `${dir}${name.endsWith(".md") ? name : `${name}.md`}`;
    setErr(null);
    try {
      await notesRename(active, target);
      setNewName("");
      setMsg(`已重命名 → ${target}（全库引用已同步）`);
      await refreshList();
      await openNote(target);
    } catch (e) {
      fail(e);
    }
  }, [active, newName, refreshList, openNote, fail]);

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
      void notesCanvasSave(canvasDir, doc).catch(() => {});
    }
  }, [canvasDir, doc]);

  const removeSelected = useCallback(() => {
    if (!selNode) return;
    void saveCanvas({
      ...doc,
      nodes: doc.nodes.filter((n) => n.id !== selNode),
      edges: doc.edges.filter((e) => e.from !== selNode && e.to !== selNode),
    });
    setSelNode(null);
  }, [selNode, doc, saveCanvas]);

  const shown = all.filter((n) => {
    const q = filter.trim().toLowerCase();
    if (!q) return true;
    return (
      n.path.toLowerCase().includes(q) ||
      n.title.toLowerCase().includes(q) ||
      n.tags.some((t) => t.toLowerCase().includes(q))
    );
  });

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <button className={`${styles.tab} ${tab === "notes" ? styles.tabActive : ""}`} onClick={() => setTab("notes")}>
          笔记（{all.length}）
        </button>
        <button className={`${styles.tab} ${tab === "review" ? styles.tabActive : ""}`} onClick={() => setTab("review")}>
          复习（{queue.length} 到期）
        </button>
        <button className={`${styles.tab} ${tab === "canvas" ? styles.tabActive : ""}`} onClick={() => setTab("canvas")}>
          画布
        </button>
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

      {msg && <Text className={styles.ok}>{msg}</Text>}
      {err && <Text className={styles.error}>{err}</Text>}
      {loading && <Spinner size="tiny" />}

      {tab === "notes" && (
        <div className={styles.section}>
          <div className={styles.row}>
            <Input
              className={styles.grow}
              placeholder="搜索标题/路径/标签"
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
          </div>
          <div className={styles.split}>
            <div className={styles.list}>
              {shown.map((n) => (
                <div
                  key={n.path}
                  className={`${styles.item} ${active === n.path ? styles.itemActive : ""}`}
                  onClick={() => void openNote(n.path)}
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
              {!loading && shown.length === 0 && <Text className={styles.muted}>暂无笔记</Text>}
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
          </div>
        </div>
      )}

      {tab === "review" && (
        <div className={styles.section}>
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
                <Button
                  size="small"
                  appearance="subtle"
                  onClick={() => void notesCardDelete(c.id).then(refreshReview).catch(fail)}
                >
                  删除
                </Button>
              </div>
            ))}
          </div>
        </div>
      )}

      {tab === "canvas" && (
        <div className={styles.section}>
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
            <Button size="small" appearance="subtle" onClick={removeSelected}>
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
        </div>
      )}
    </div>
  );
}
