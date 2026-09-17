import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Spinner,
} from "@fluentui/react-components";
import { marked } from "marked";
import {
  editorAutosave,
  editorClose,
  editorContent,
  editorOpen,
  editorSave,
  editorSessions,
  pdfCompress,
  pdfInfo,
  pdfMerge,
  pdfSplit,
  pdfWatermark,
  parseAppError,
  type EditorSessionInfoDto,
  type PdfInfoDto,
} from "../../ipc/client";
import { languageForPath, monaco } from "../../monaco/setup";

/**
 * 文本与 PDF 面板（docs/impl/06 E1–E4，M9 v1）：
 * - E1 会话：路径打开、脏标记、编码/EOL 徽标（混合行尾保存前明示）
 * - E2 Monaco：>5MB 关语法高亮+minimap（model 语言 plaintext）、>50MB 只读；
 *   脏后 3s 防抖 autosave（.nforge-autosave 崩溃恢复草稿）
 * - E3 Markdown 分栏预览（滚动比例同步）
 * - E4 PDF：合并/拆分/压缩/水印（lopdf，压缩结果更大自动保留原文件）
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
  ok: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
  warn: { color: tokens.colorPaletteMarigoldForeground1, fontSize: tokens.fontSizeBase200 },
  tabs: { display: "flex", gap: "4px", flexWrap: "wrap" },
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
  tabActive: { backgroundColor: tokens.colorNeutralBackground3Hover, border: `1px solid ${tokens.colorBrandForeground1}` },
  editorWrap: {
    display: "grid",
    gridTemplateColumns: "1fr 1fr",
    gap: "8px",
    height: "460px",
  },
  editorSingle: { display: "flex", height: "460px" },
  editorHost: { flex: 1, minWidth: 0, border: `1px solid ${tokens.colorNeutralStroke1}`, borderRadius: tokens.borderRadiusMedium },
  preview: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "12px 16px",
    backgroundColor: tokens.colorNeutralBackground2,
    fontSize: tokens.fontSizeBase300,
  },
});

export default function EditorPanel() {
  const styles = useStyles();
  const [openPath, setOpenPath] = useState("");
  const [sessions, setSessions] = useState<EditorSessionInfoDto[]>([]);
  const [activeId, setActiveId] = useState("");
  const [error, setError] = useState("");
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState("");
  const [mdPreview, setMdPreview] = useState(true);
  const [mdHtml, setMdHtml] = useState("");

  const editorHostRef = useRef<HTMLDivElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const modelsRef = useRef<Map<string, monaco.editor.ITextModel>>(new Map());
  const autosaveTimer = useRef<number | null>(null);
  const mounted = useRef(true);

  // Monaco 编辑器创建（一次）
  useEffect(() => {
    if (!editorHostRef.current || editorRef.current) return;
    editorRef.current = monaco.editor.create(editorHostRef.current, {
      theme: "vs",
      minimap: { enabled: true },
      automaticLayout: true,
      fontSize: 13,
    });
    const ed = editorRef.current;
    // 内容变更 → 脏标记 + 3s 防抖 autosave（E2 崩溃恢复入口）
    ed.onDidChangeModelContent(() => {
      const model = ed.getModel();
      if (!activeId || model === null) return;
      const session = sessions.find((s) => s.id === activeId);
      if (session?.readonly) return; // >50MB 只读：不置脏不存草稿
      setSessions((prev) => prev.map((s) => (s.id === activeId ? { ...s, dirty: true } : s)));
      if (autosaveTimer.current) window.clearTimeout(autosaveTimer.current);
      autosaveTimer.current = window.setTimeout(() => {
        void editorAutosave(activeId, model?.getValue() ?? "").catch(() => undefined);
      }, 3000);
    });
    return () => {
      editorRef.current?.dispose();
      editorRef.current = null;
      modelsRef.current.forEach((m) => m.dispose());
      modelsRef.current.clear();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, sessions]);

  // 切换会话：换 model + 更新降级配置 + MD 预览内容
  useEffect(() => {
    const ed = editorRef.current;
    const session = sessions.find((s) => s.id === activeId);
    if (!ed || !session) return;
    const key = `${session.id}|${session.path}`;
    let model = modelsRef.current.get(key);
    if (!model) {
      model = monaco.editor.createModel("", languageForPath(session.path));
      modelsRef.current.set(key, model);
      void editorContent(session.id)
        .then((text) => {
          if (model && !model.isDisposed()) model.setValue(text);
        })
        .catch((e) => setError(parseAppError(e)?.data.message ?? String(e)));
    }
    // E2 大文件降级：>5MB 关高亮（plaintext）+ minimap；>50MB 只读
    if (session.big_file) {
      if (model.getLanguageId() !== "plaintext") monaco.editor.setModelLanguage(model, "plaintext");
      ed.updateOptions({ minimap: { enabled: false }, readOnly: session.readonly });
    } else {
      ed.updateOptions({ minimap: { enabled: true }, readOnly: session.readonly });
    }
    ed.setModel(model);
    // E3 Markdown 预览
    if (session.path.toLowerCase().endsWith(".md") && mdPreview) {
      const render = () => {
        if (model && !model.isDisposed()) setMdHtml(marked.parse(model.getValue()) as string);
      };
      render();
      const disp = model.onDidChangeContent(render);
      return () => disp.dispose();
    }
    setMdHtml("");
    return undefined;
  }, [activeId, sessions, mdPreview]);

  // E3 分栏滚动同步（比例同步）
  useEffect(() => {
    const ed = editorRef.current;
    const preview = previewRef.current;
    if (!ed || !preview || !mdHtml) return;
    let syncing = false;
    const edDisp = ed.onDidScrollChange(() => {
      if (syncing) return;
      syncing = true;
      const ratio = ed.getScrollTop() / Math.max(1, ed.getScrollHeight() - ed.getLayoutInfo().height);
      preview.scrollTop = ratio * (preview.scrollHeight - preview.clientHeight);
      requestAnimationFrame(() => {
        syncing = false;
      });
    });
    const pvDisp = preview.addEventListener("scroll", () => {
      if (syncing) return;
      syncing = true;
      const ratio = preview.scrollTop / Math.max(1, preview.scrollHeight - preview.clientHeight);
      ed.setScrollTop(ratio * (ed.getScrollHeight() - ed.getLayoutInfo().height));
      requestAnimationFrame(() => {
        syncing = false;
      });
    });
    return () => {
      edDisp.dispose();
      preview.removeEventListener("scroll", pvDisp as unknown as EventListener);
    };
  }, [mdHtml, activeId]);

  const refreshSessions = useCallback(async () => {
    const list = await editorSessions();
    if (mounted.current) setSessions(list);
  }, []);

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

  const doOpen = () =>
    run("open", async () => {
      const info = await editorOpen(openPath.trim());
      await refreshSessions();
      setActiveId(info.id);
      setStatus(`已打开 ${info.name}（${info.encoding_label}）`);
    });

  const doSave = (id: string) =>
    run(`save-${id}`, async () => {
      const info = await editorSave(id);
      await refreshSessions();
      setStatus(
        info.eol_mixed
          ? "已保存（行尾已整文件统一）"
          : `已保存 ${info.name}（${info.encoding_label} / ${info.eol.toUpperCase()}）`,
      );
    });

  const doClose = (id: string) =>
    run(`close-${id}`, async () => {
      await editorClose(id);
      if (activeId === id) setActiveId("");
      await refreshSessions();
    });

  // Ctrl+S 保存当前会话
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s" && activeId) {
        e.preventDefault();
        void doSave(activeId);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId]);

  const active = sessions.find((s) => s.id === activeId);

  // ---------------- PDF 工具状态 ----------------
  const [pdfPath, setPdfPath] = useState("");
  const [pdfInputs, setPdfInputs] = useState("");
  const [pdfOut, setPdfOut] = useState("");
  const [pdfInfoState, setPdfInfoState] = useState<PdfInfoDto | null>(null);
  const [wmText, setWmText] = useState("");

  const pdfRun = (key: string, action: () => Promise<unknown>) =>
    run(key, async () => {
      await action();
      if (pdfPath.trim()) setPdfInfoState(await pdfInfo(pdfPath.trim()));
    });

  return (
    <div className={styles.root}>
      {/* E1 会话管理 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">文本编辑</Text>
          {active && (
            <>
              <Badge appearance="outline">{active.encoding_label}</Badge>
              <Badge appearance="outline">{active.eol.toUpperCase()}</Badge>
              {active.dirty && <Badge appearance="filled" color="warning">未保存</Badge>}
              {active.big_file && <Badge appearance="outline">大文件·已关高亮</Badge>}
              {active.readonly && <Badge appearance="filled" color="danger">只读</Badge>}
              {active.eol_mixed && (
                <span className={styles.warn}>检测到混合行尾，保存将整文件统一为 {active.eol.toUpperCase()}</span>
              )}
            </>
          )}
          <span className={styles.grow} />
          {active && active.dirty && (
            <Button size="small" appearance="primary" disabled={busy !== ""} onClick={() => doSave(active.id)}>
              保存（Ctrl+S）
            </Button>
          )}
        </div>
        <div className={styles.row}>
          <Input
            className={styles.grow}
            placeholder="文件绝对路径（如 C:\\Users\\me\\notes.md）"
            value={openPath}
            onChange={(_, d) => setOpenPath(d.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && openPath.trim()) void doOpen();
            }}
            size="small"
          />
          <Button size="small" appearance="primary" disabled={busy !== "" || openPath.trim() === ""} onClick={doOpen}>
            打开
          </Button>
          {active && active.path.toLowerCase().endsWith(".md") && (
            <Button size="small" onClick={() => setMdPreview((v) => !v)}>
              {mdPreview ? "隐藏预览" : "显示预览"}
            </Button>
          )}
        </div>
        {sessions.length > 0 && (
          <div className={styles.tabs}>
            {sessions.map((s) => (
              <div
                key={s.id}
                className={`${styles.tab} ${s.id === activeId ? styles.tabActive : ""}`}
                onClick={() => setActiveId(s.id)}
              >
                {s.dirty && <span style={{ color: tokens.colorPaletteMarigoldForeground1 }}>●</span>}
                {s.name}
                <span
                  role="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    void doClose(s.id);
                  }}
                >
                  ✕
                </span>
              </div>
            ))}
          </div>
        )}
        {active ? (
          <div className={mdPreview && mdHtml ? styles.editorWrap : styles.editorSingle}>
            <div ref={editorHostRef} className={styles.editorHost} />
            {mdPreview && mdHtml ? (
              <div
                ref={previewRef}
                className={styles.preview}
                /* marked 输出经 sanitize 需求：v1 内容来自用户本地文件，与编辑器同信任域 */
                dangerouslySetInnerHTML={{ __html: mdHtml }}
              />
            ) : null}
          </div>
        ) : (
          <span className={styles.muted}>
            输入路径打开文件；&gt;5MB 自动关闭语法高亮，&gt;50MB 只读；修改后 3s 自动存草稿（.nforge-autosave）
          </span>
        )}
      </div>

      {/* E4 PDF 工具 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">PDF 工具</Text>
          {pdfInfoState && (
            <Badge appearance="outline">
              {pdfInfoState.pages} 页 · {(pdfInfoState.size / 1024).toFixed(0)} KB
            </Badge>
          )}
        </div>
        <div className={styles.row}>
          <Input
            className={styles.grow}
            placeholder="PDF 绝对路径"
            value={pdfPath}
            onChange={(_, d) => setPdfPath(d.value)}
            size="small"
          />
          <Button
            size="small"
            disabled={busy !== "" || pdfPath.trim() === ""}
            onClick={() => pdfRun("pdf-info", async () => setPdfInfoState(await pdfInfo(pdfPath.trim())))}
          >
            信息
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || pdfPath.trim() === ""}
            onClick={() =>
              pdfRun("pdf-split", async () => {
                const r = await pdfSplit(pdfPath.trim(), pdfPath.trim() + "_pages");
                setStatus(`拆分完成：${r.length} 个单页文件`);
              })
            }
          >
            拆分
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || pdfPath.trim() === ""}
            onClick={() =>
              pdfRun("pdf-compress", async () => {
                const r = await pdfCompress(pdfPath.trim());
                setStatus(`压缩完成：${(r.size / 1024).toFixed(0)} KB（若更大已自动保留原文件）`);
              })
            }
          >
            压缩
          </Button>
          <Input
            className={styles.grow}
            placeholder="水印文本"
            value={wmText}
            onChange={(_, d) => setWmText(d.value)}
            size="small"
          />
          <Button
            size="small"
            disabled={busy !== "" || pdfPath.trim() === "" || wmText.trim() === ""}
            onClick={() =>
              pdfRun("pdf-wm", async () => {
                await pdfWatermark(pdfPath.trim(), wmText.trim());
                setStatus("水印已写入");
              })
            }
          >
            水印
          </Button>
        </div>
        <div className={styles.row}>
          <Input
            className={styles.grow}
            placeholder="合并输入：多个 PDF 路径用 | 分隔"
            value={pdfInputs}
            onChange={(_, d) => setPdfInputs(d.value)}
            size="small"
          />
          <Input
            className={styles.grow}
            placeholder="合并输出路径"
            value={pdfOut}
            onChange={(_, d) => setPdfOut(d.value)}
            size="small"
          />
          <Button
            size="small"
            disabled={busy !== "" || pdfInputs.trim() === "" || pdfOut.trim() === ""}
            onClick={() =>
              pdfRun("pdf-merge", async () => {
                const inputs = pdfInputs.split("|").map((s) => s.trim()).filter(Boolean);
                const r = await pdfMerge(inputs, pdfOut.trim());
                setStatus(`合并完成：${r.pages} 页`);
              })
            }
          >
            合并
          </Button>
        </div>
      </div>

      {busy && <Spinner size="tiny" label="处理中…" />}
      {status && <span className={styles.ok}>{status}</span>}
      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
}
