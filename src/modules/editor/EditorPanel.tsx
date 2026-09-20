import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
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
  editorSaveAs,
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
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { languageForPath, monaco } from "../../monaco/setup";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 文本与 PDF 面板（docs/impl/06 E1–E4，M9 v1）：
 * - E1 会话：路径打开、脏标记、编码/EOL 徽标（混合行尾保存前明示）
 * - E2 Monaco：>5MB 关语法高亮+minimap（model 语言 plaintext）、>50MB 只读；
 *   脏后 3s 防抖 autosave（.nforge-autosave 崩溃恢复草稿）
 * - E3 Markdown 分栏预览（滚动比例同步）
 * - E4 PDF：合并/拆分/压缩/水印（lopdf，压缩结果更大自动保留原文件）
 * - D-18：脏缓冲区关闭、PDF 原地改写（压缩/水印）一律经 confirmAction 二次确认
 * - T-B1-10：另存为——目标路径 Input 沿用打开惯例，后端 save_as 换绑会话 path，
 *   页签名经 refreshSessions（同 editorSessions 真相源）刷新
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
  warn: { color: tokens.colorPaletteMarigoldForeground1, fontSize: tokens.fontSizeBase200 },
  tabs: { display: "flex", gap: "4px", flexWrap: "wrap" },
  // 标签胶囊 = 容器（边框/底色）+ 真 button 标签 + 真 button 关闭钮：
  // 关闭钮不能嵌套在标签 button 内（按钮内不可有交互内容），故外层为容器而非 button
  tab: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "4px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    fontSize: tokens.fontSizeBase200,
  },
  tabActive: { backgroundColor: tokens.colorNeutralBackground3Hover, border: `1px solid ${tokens.colorBrandForeground1}` },
  tabLabel: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    minWidth: 0,
    padding: 0,
    border: "none",
    backgroundColor: "transparent",
    color: "inherit",
    fontFamily: "inherit",
    fontSize: "inherit",
    cursor: "pointer",
  },
  tabClose: {
    padding: "0 2px",
    border: "none",
    borderRadius: tokens.borderRadiusSmall,
    backgroundColor: "transparent",
    color: tokens.colorNeutralForeground3,
    fontFamily: "inherit",
    fontSize: "inherit",
    lineHeight: "1",
    cursor: "pointer",
    ":hover": { color: tokens.colorPaletteRedForeground1 },
  },
  dirtyDot: { color: tokens.colorPaletteMarigoldForeground1 },
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

/** 确认框影响面点名（D-18）：绝对路径 → 文件名，兼容 \ 与 / */
function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

export default function EditorPanel() {
  const styles = useStyles();
  const [openPath, setOpenPath] = useState("");
  const [saveAsPath, setSaveAsPath] = useState("");
  const [sessions, setSessions] = useState<EditorSessionInfoDto[]>([]);
  const [activeId, setActiveId] = useState("");
  const [error, setError] = useState("");
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState("");
  const [mdPreview, setMdPreview] = useState(true);
  const [mdHtml, setMdHtml] = useState("");
  // 首轮会话列表是否落定（成功或失败）：未落定前编辑区渲染加载态而非引导文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);

  const editorHostRef = useRef<HTMLDivElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const modelsRef = useRef<Map<string, monaco.editor.ITextModel>>(new Map());
  const autosaveTimer = useRef<number | null>(null);
  const mounted = useRef(true);
  // 编辑器只创建一次（见下方 effect），内容变更监听器闭包不随渲染更新，
  // 最新会话事实经这两个 ref 桥接；程序化载入用 loadingLoadRef 抑制置脏。
  const activeIdRef = useRef("");
  const sessionsRef = useRef(sessions);
  const loadingLoadRef = useRef(false);
  activeIdRef.current = activeId;
  sessionsRef.current = sessions;

  // Monaco 编辑器创建（deps 曾为 [activeId, sessions]——每次会话/脏标记变化都
  // dispose 重建编辑器与全部 model，实启冒烟证实这会把在途 autosave 定时器留在已 dispose
  // 的 model 上抛 "Model is disposed!"，故监听器一律经 ref 读当前值）。
  // 宿主编辑区是条件渲染（{active ? ...}，无活跃会话时挂点不存在），deps 取
  // 「是否存在活跃会话」这一布尔：false→true 建一次，其后切会话/置脏布尔不变即不再重建；
  // 全部关闭→重开才走 dispose/重建（editorRef.current 守卫兜底 StrictMode 双跑）。
  useEffect(() => {
    if (!editorHostRef.current || editorRef.current) return;
    editorRef.current = monaco.editor.create(editorHostRef.current, {
      theme: "vs",
      minimap: { enabled: true },
      automaticLayout: true,
      fontSize: 13,
    });
    const ed = editorRef.current;
    // modelsRef 从不重赋值，cleanup 经局部变量访问（exhaustive-deps 要求）
    const models = modelsRef.current;
    // 内容变更 → 脏标记 + 3s 防抖 autosave（E2 崩溃恢复入口）
    ed.onDidChangeModelContent(() => {
      const model = ed.getModel();
      const id = activeIdRef.current;
      if (!id || model === null || loadingLoadRef.current) return;
      const session = sessionsRef.current.find((s) => s.id === id);
      if (session?.readonly) return; // >50MB 只读：不置脏不存草稿
      setSessions((prev) => prev.map((s) => (s.id === id ? { ...s, dirty: true } : s)));
      if (autosaveTimer.current) window.clearTimeout(autosaveTimer.current);
      autosaveTimer.current = window.setTimeout(() => {
        // 3s 窗口内 model 可能随会话换绑/卸载被 dispose，getValue() 会抛
        // "Model is disposed!"（未捕获 → 全局错误通道刷日志）
        if (model.isDisposed()) return;
        void editorAutosave(id, model.getValue()).catch((e) =>
          reportError(e, { context: "草稿自动保存失败", dedupeKey: "editor-autosave", toast: false }),
        );
      }, 3000);
    });
    return () => {
      if (autosaveTimer.current) window.clearTimeout(autosaveTimer.current);
      autosaveTimer.current = null;
      editorRef.current?.dispose();
      editorRef.current = null;
      models.forEach((m) => m.dispose());
      models.clear();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 布尔表达式即上方纪律的全部依赖语义；会话事实经 ref 桥接
  }, [sessions.some((s) => s.id === activeId)]);

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
          if (model && !model.isDisposed()) {
            // 程序化载入不是用户编辑：monaco 变更事件同步派发，抑制置脏与草稿
            loadingLoadRef.current = true;
            try {
              model.setValue(text);
            } finally {
              loadingLoadRef.current = false;
            }
          }
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
    try {
      const list = await editorSessions();
      if (mounted.current) setSessions(list);
    } finally {
      // 失败同样算"已落定"：调用方（run/挂载 effect）负责把错误显示出来
      if (mounted.current) setLoaded(true);
    }
  }, []);

  // 挂载即拉取会话列表：此前仅在 open/save/close 之后刷新，重进面板时标签条与后端实态不符
  useEffect(() => {
    void refreshSessions().catch((e) => setError(parseAppError(e)?.data.message ?? String(e)));
  }, [refreshSessions]);

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

  // 另存为（T-B1-10）：后端 save_as 把会话 path 换绑到目标并返回新 SessionInfo，
  // 页签按面板惯例经 refreshSessions（editorSessions 真相源）刷新；目标路径走与打开
  // 同一 Input 惯例（trim + Enter 触发）。目标若已存在将被覆写——路径由用户亲手输入。
  const doSaveAs = (id: string) =>
    run(`save-as-${id}`, async () => {
      const target = saveAsPath.trim();
      const info = await editorSaveAs(id, target);
      await refreshSessions();
      setSaveAsPath("");
      setStatus(`已另存为 ${info.name}（${info.encoding_label} / ${info.eol.toUpperCase()}）`);
    });

  // 关闭会话（D-18）：后端 close() 会销毁缓冲区并删除 .nforge-autosave 草稿，
  // 未保存内容彻底丢失 → 脏缓冲区必须二次确认，干净的直接关不打扰。
  const doClose = async (s: EditorSessionInfoDto) => {
    if (
      s.dirty &&
      !(await confirmAction({
        title: "关闭未保存的缓冲区",
        impact: [`「${s.name}」有未保存修改，关闭将丢失更改`],
        detail: "关闭会销毁编辑器缓冲区并删除 .nforge-autosave 草稿文件，磁盘上仍是上次保存的内容。",
        confirmLabel: "放弃并关闭",
      }))
    )
      return;
    await run(`close-${s.id}`, async () => {
      await editorClose(s.id);
      if (activeId === s.id) setActiveId("");
      await refreshSessions();
    });
  };

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
    // doSave 为每次渲染新建的普通函数（仅闭包稳定引用），入依赖表将每帧重挂监听；
    // activeId 在表内已保证读到当前会话
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

  // 压缩（D-18）：有收益时新文件 rename 覆盖源 PDF 且不留备份 → 先确认
  const doCompress = async () => {
    const path = pdfPath.trim();
    if (
      !(await confirmAction({
        title: "压缩 PDF",
        impact: [`将原地重写「${baseName(path)}」`],
        detail: "压缩有收益时结果文件直接替换原文件且不保留备份；若变大则自动保留原文件。",
        confirmLabel: "压缩",
      }))
    )
      return;
    await pdfRun("pdf-compress", async () => {
      const r = await pdfCompress(path);
      setStatus(`压缩完成：${(r.size / 1024).toFixed(0)} KB（若更大已自动保留原文件）`);
    });
  };

  // 水印（D-18）：逐页盖印后直接 save 回源路径，属不可逆的原文件改写 → 先确认
  const doWatermark = async () => {
    const path = pdfPath.trim();
    const text = wmText.trim();
    if (
      !(await confirmAction({
        title: "写入 PDF 水印",
        impact: [
          `将逐页叠加水印并覆盖保存「${baseName(path)}」`,
          `水印文本：${text}`,
          pdfInfoState ? `影响页数：${pdfInfoState.pages} 页` : "页数未知：可先取消并点「信息」读取",
        ],
        detail: "水印写回源文件本身，不生成副本，写入后无法撤销。",
        confirmLabel: "写入水印",
      }))
    )
      return;
    await pdfRun("pdf-wm", async () => {
      await pdfWatermark(path, text);
      setStatus("水印已写入");
    });
  };

  return (
    <div className={styles.root}>
      {/* E1 会话管理 */}
      <Section
        title="文本编辑"
        actions={
          <>
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
            {active && active.dirty && (
              <Button size="small" appearance="primary" disabled={busy !== ""} onClick={() => doSave(active.id)}>
                保存（Ctrl+S）
              </Button>
            )}
            {active && (
              <Button
                size="small"
                disabled={busy !== "" || saveAsPath.trim() === ""}
                title="目标路径填在下方输入框；另存后会话换绑到新文件"
                onClick={() => doSaveAs(active.id)}
              >
                另存为
              </Button>
            )}
          </>
        }
      >
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
          {active && (
            <Input
              className={styles.grow}
              placeholder="另存为目标绝对路径（填好后点右上方「另存为」，Enter 亦可）"
              value={saveAsPath}
              onChange={(_, d) => setSaveAsPath(d.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && saveAsPath.trim()) void doSaveAs(active.id);
              }}
              size="small"
            />
          )}
          {active && active.path.toLowerCase().endsWith(".md") && (
            <Button size="small" onClick={() => setMdPreview((v) => !v)}>
              {mdPreview ? "隐藏预览" : "显示预览"}
            </Button>
          )}
        </div>
        {sessions.length > 0 && (
          <div className={styles.tabs} role="tablist" aria-label="已打开的缓冲区">
            {sessions.map((s) => (
              <div key={s.id} className={`${styles.tab} ${s.id === activeId ? styles.tabActive : ""}`}>
                <button
                  type="button"
                  role="tab"
                  aria-selected={s.id === activeId}
                  className={styles.tabLabel}
                  onClick={() => setActiveId(s.id)}
                >
                  {s.dirty && <span className={styles.dirtyDot}>●</span>}
                  {s.name}
                </button>
                <button
                  type="button"
                  className={styles.tabClose}
                  aria-label={`关闭 ${s.name}`}
                  onClick={() => void doClose(s)}
                >
                  ✕
                </button>
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
          <EmptyState
            text="输入路径打开文件；>5MB 自动关闭语法高亮，>50MB 只读；修改后 3s 自动存草稿（.nforge-autosave）"
            loading={!loaded}
          />
        )}
      </Section>

      {/* E4 PDF 工具 */}
      <Section
        title="PDF 工具"
        actions={
          pdfInfoState && (
            <Badge appearance="outline">
              {pdfInfoState.pages} 页 · {(pdfInfoState.size / 1024).toFixed(0)} KB
            </Badge>
          )
        }
      >
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
            onClick={() => void doCompress()}
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
            onClick={() => void doWatermark()}
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
      </Section>

      {busy && <Spinner size="tiny" label="处理中…" />}
      <InlineError text={status} tone="success" />
      <InlineError text={error} />
    </div>
  );
}
