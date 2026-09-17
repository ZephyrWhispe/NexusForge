import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Select,
  Table,
  TableBody,
  TableCell,
  TableRow,
  Tooltip,
  ProgressBar,
} from "@fluentui/react-components";
import {
  fileBreadcrumbs,
  fileDrives,
  fileList,
  fileEnqueue,
  fileMkdir,
  fileOpsActive,
  fileOpCancel,
  fileOpPause,
  fileOpResume,
  parseAppError,
  type ConflictItemDto,
  type ConflictPolicyDto,
  type FileEntryDto,
  type OpProgressDto,
} from "../../ipc/client";

/**
 * 文件与存储面板（docs/impl/05 F，M6 v1）：
 * ① 盘符/面包屑/目录列表导航 ② 选中复制/移动/删除入队（Ask 冲突预扫描）
 * ③ operation.progress 事件驱动的操作队列 ④ 新建目录。
 */
const useStyles = makeStyles({
  root: {
    flex: 1,
    minWidth: 0,
    overflow: "hidden",
    padding: "0 20px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "12px",
  },
  toolbar: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  crumbs: { display: "flex", alignItems: "center", gap: "4px", flexWrap: "wrap" },
  crumbBtn: { padding: "2px 6px", minWidth: 0 },
  sep: { color: tokens.colorNeutralForeground3 },
  tableWrap: {
    flex: 1,
    minHeight: 0,
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  row: { cursor: "default" },
  rowSelected: { backgroundColor: tokens.colorBrandBackground2 },
  nameCell: { maxWidth: "360px", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  queue: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    backgroundColor: tokens.colorNeutralBackground2,
  },
  opRow: { display: "flex", alignItems: "center", gap: "8px" },
  bar: { flex: 1, minWidth: "120px" },
  err: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  conflictBox: {
    border: `1px solid ${tokens.colorPaletteYellowBorder1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    backgroundColor: tokens.colorPaletteYellowBackground1,
  },
});

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTime(ms: number): string {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toLocaleString("zh-CN", { hour12: false });
}

const OP_KIND_LABEL: Record<string, string> = {
  copy: "复制",
  move: "移动",
  delete: "删除",
  compress: "压缩",
  extract: "解压",
};

export default function FilePanel() {
  const styles = useStyles();
  const [cwd, setCwd] = useState<string | null>(null);
  const [crumbs, setCrumbs] = useState<[string, string][]>([]);
  const [entries, setEntries] = useState<FileEntryDto[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [conflicts, setConflicts] = useState<ConflictItemDto[] | null>(null);
  const [pendingSpec, setPendingSpec] = useState<{ kind: "copy" | "move"; dst: string } | null>(null);
  const [ops, setOps] = useState<OpProgressDto[]>([]);
  const [mkdirName, setMkdirName] = useState("");
  const [dstInput, setDstInput] = useState("");
  const [drives, setDrives] = useState<[string, string][]>([]);
  const opsRef = useRef<Map<string, OpProgressDto>>(new Map());

  const applyError = useCallback((e: unknown, fallback: string) => {
    const err = parseAppError(e);
    setError(err ? `${err.data.code}: ${err.data.message}` : fallback);
  }, []);

  const loadDir = useCallback(
    async (path: string) => {
      try {
        const [list, bc] = await Promise.all([fileList(path), fileBreadcrumbs(path)]);
        setEntries(list);
        setCrumbs(bc.map(([n, p]) => [n, String(p)] as [string, string]));
        setCwd(path);
        setSelected(new Set());
        setError(null);
      } catch (e) {
        applyError(e, "目录读取失败");
      }
    },
    [applyError],
  );

  // 初始定位到主目录；加载盘符下拉
  useEffect(() => {
    void loadDir("C:\\").catch(() => undefined);
    fileDrives()
      .then((ds) => setDrives(ds.map((d) => [d.letter, String(d.path)] as [string, string])))
      .catch(() => undefined);
  }, [loadDir]);

  const refreshOps = useCallback(async () => {
    try {
      const list = await fileOpsActive();
      for (const p of list) opsRef.current.set(p.op_id, p);
      // 只保留近端（Done/Failed 保留至下一次刷新窗口）
      setOps(list.slice(-8));
    } catch {
      /* 模块未就绪时静默 */
    }
  }, []);

  // operation.progress 事件驱动刷新（节流由后端保证 200ms）
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<{ topic: string }>("nf:event", (e) => {
          const topic = e.payload?.topic;
          if (
            topic === "operation.progress" ||
            topic === "operation.done" ||
            topic === "operation.failed"
          ) {
            void refreshOps();
          }
        }),
      )
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refreshOps]);

  const toggleSelect = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const openEntry = (e: FileEntryDto) => {
    if (e.is_dir) void loadDir(e.path);
  };

  const enqueue = async (
    kind: "copy" | "move" | "delete",
    dst = "",
    policy: ConflictPolicyDto = "ask",
  ) => {
    if (!cwd || selected.size === 0) return;
    if ((kind === "copy" || kind === "move") && !dstInput.trim()) {
      setError("请在「目标目录」输入框填写目标路径");
      return;
    }
    const target = kind === "delete" ? dst : dstInput.trim();
    try {
      const res = await fileEnqueue({
        kind,
        srcs: [...selected],
        dst: target,
        policy,
        recycle: kind === "delete",
      });
      if (res.conflicts.length > 0 && res.op_id === null) {
        setConflicts(res.conflicts);
        setPendingSpec({ kind: kind as "copy" | "move", dst: target });
        return;
      }
      setError(null);
      void refreshOps();
    } catch (e) {
      applyError(e, "操作入队失败");
    }
  };

  const resolveConflicts = async (policy: "skip" | "overwrite" | "rename") => {
    if (!pendingSpec) return;
    setConflicts(null);
    const spec = pendingSpec;
    setPendingSpec(null);
    try {
      await fileEnqueue({ kind: spec.kind, srcs: [...selected], dst: spec.dst, policy });
      void refreshOps();
    } catch (e) {
      applyError(e, "操作入队失败");
    }
  };

  const doMkdir = async () => {
    if (!cwd || !mkdirName.trim()) return;
    try {
      await fileMkdir([cwd, mkdirName.trim()].join("\\"));
      setMkdirName("");
      void loadDir(cwd);
    } catch (e) {
      applyError(e, "新建目录失败");
    }
  };

  const opControl = async (p: OpProgressDto, action: "pause" | "resume" | "cancel") => {
    try {
      if (action === "pause") await fileOpPause(p.op_id);
      else if (action === "cancel") await fileOpCancel(p.op_id);
      else await fileOpResume(p.op_id);
      void refreshOps();
    } catch (e) {
      applyError(e, "操作控制失败");
    }
  };

  const activeOps = ops.filter((p) =>
    ["Queued", "Running", "Paused"].includes(p.state),
  );
  const finishedOps = ops.filter((p) =>
    ["Done", "Failed", "Canceled"].includes(p.state),
  );

  return (
    <div className={styles.root}>
      {/* 导航栏 */}
      <div className={styles.toolbar}>
        <div className={styles.crumbs}>
          {crumbs.map(([name, path], i) => (
            <span key={path} className={styles.crumbs}>
              {i > 0 && <span className={styles.sep}>›</span>}
              <Button
                appearance="subtle"
                size="small"
                className={styles.crumbBtn}
                onClick={() => void loadDir(path)}
              >
                {name}
              </Button>
            </span>
          ))}
        </div>
        <Select
          appearance="outline"
          value=""
          onChange={(_, d) => d.value && void loadDir(d.value)}
          style={{ maxWidth: "140px" }}
        >
          <option value="" disabled>
            盘符
          </option>
          {drives.map(([letter, path]) => (
            <option key={letter} value={path}>
              {letter}
            </option>
          ))}
        </Select>
        <Input
          size="small"
          placeholder="新建目录名"
          value={mkdirName}
          onChange={(_, d) => setMkdirName(d.value)}
          style={{ maxWidth: "160px" }}
        />
        <Input
          size="small"
          placeholder="目标目录（复制/移动用）"
          value={dstInput}
          onChange={(_, d) => setDstInput(d.value)}
          style={{ maxWidth: "240px" }}
        />
        <Button size="small" onClick={() => void doMkdir()}>
          新建
        </Button>
      </div>

      {/* 操作栏 */}
      <div className={styles.toolbar}>
        <span className={styles.muted}>已选 {selected.size} 项</span>
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size === 0}
          onClick={() => void enqueue("copy")}
        >
          复制到…
        </Button>
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size === 0}
          onClick={() => void enqueue("move")}
        >
          移动到…
        </Button>
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size === 0}
          onClick={() => void enqueue("delete")}
        >
          删除
        </Button>
        <span style={{ flex: 1 }} />
        {error && <span className={styles.err}>{error}</span>}
      </div>

      {/* 冲突决议（F3：Ask 预扫描，"应用到全部"） */}
      {conflicts && (
        <div className={styles.conflictBox}>
          <Text weight="semibold" size={300}>
            {conflicts.length} 个同名冲突（目标：{pendingSpec?.dst}）
          </Text>
          {conflicts.slice(0, 5).map((c) => (
            <Text key={c.dst} size={200} className={styles.muted}>
              {c.name} → {c.dst}
            </Text>
          ))}
          <div className={styles.toolbar}>
            <Button size="small" onClick={() => void resolveConflicts("rename")}>
              重命名保留两者
            </Button>
            <Button size="small" onClick={() => void resolveConflicts("overwrite")}>
              全部覆盖
            </Button>
            <Button size="small" onClick={() => void resolveConflicts("skip")}>
              全部跳过
            </Button>
            <Button
              size="small"
              appearance="subtle"
              onClick={() => {
                setConflicts(null);
                setPendingSpec(null);
              }}
            >
              取消
            </Button>
          </div>
        </div>
      )}

      {/* 操作队列（F2：进度 + 暂停/恢复/取消） */}
      {activeOps.length > 0 && (
        <div className={styles.queue}>
          {activeOps.map((p) => {
            const ratio = p.bytes_total > 0 ? p.bytes_done / p.bytes_total : 0;
            return (
              <div key={p.op_id} className={styles.opRow}>
                <Badge appearance="outline">{OP_KIND_LABEL[p.kind] ?? p.kind}</Badge>
                <ProgressBar className={styles.bar} value={Math.min(1, Math.max(0, ratio))} />
                <span className={styles.muted}>
                  {fmtSize(p.bytes_done)} / {fmtSize(p.bytes_total)} · {p.files_done}/
                  {p.files_total}
                  {p.current ? ` · ${p.current}` : ""}
                  {p.state === "Paused" ? " · 已暂停" : ""}
                </span>
                {p.state === "Running" || p.state === "Queued" ? (
                  <Button size="small" onClick={() => void opControl(p, "pause")}>
                    暂停
                  </Button>
                ) : (
                  <Button size="small" onClick={() => void opControl(p, "resume")}>
                    恢复
                  </Button>
                )}
                <Button size="small" onClick={() => void opControl(p, "cancel")}>
                  取消
                </Button>
              </div>
            );
          })}
        </div>
      )}

      {/* 目录列表 */}
      <div className={styles.tableWrap}>
        <Table>
          <TableBody>
            {entries.map((e) => {
              const isSel = selected.has(e.path);
              return (
                <TableRow
                  key={e.path}
                  className={`${styles.row} ${isSel ? styles.rowSelected : ""}`}
                  onClick={() => toggleSelect(e.path)}
                  onDoubleClick={() => openEntry(e)}
                >
                  <TableCell className={styles.nameCell}>
                    {e.is_dir ? "📁 " : "📄 "}
                    {e.name}
                    {e.hidden ? " (隐藏)" : ""}
                  </TableCell>
                  <TableCell>
                    <Text size={200} className={styles.muted}>
                      {e.is_dir ? "目录" : fmtSize(e.size)}
                    </Text>
                  </TableCell>
                  <TableCell>
                    <Tooltip content={fmtTime(e.modified_ms)} relationship="label">
                      <Text size={200} className={styles.muted}>
                        {fmtTime(e.modified_ms)}
                      </Text>
                    </Tooltip>
                  </TableCell>
                </TableRow>
              );
            })}
            {entries.length === 0 && (
              <TableRow>
                <TableCell>
                  <Text size={300} className={styles.muted}>
                    {cwd ? "目录为空" : "正在加载…"}
                  </Text>
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>

      {/* 近期完成（终态摘要） */}
      {finishedOps.length > 0 && (
        <div className={styles.toolbar}>
          {finishedOps.map((p) => (
            <Badge key={p.op_id} appearance={p.state === "Done" ? "filled" : "outline"} color={p.state === "Done" ? "success" : "danger"}>
              {OP_KIND_LABEL[p.kind]} · {p.state}
              {p.error ? ` · ${p.error}` : ""}
            </Badge>
          ))}
          <Button
            size="small"
            appearance="subtle"
            onClick={() => {
              opsRef.current.clear();
              setOps([]);
            }}
          >
            清除
          </Button>
        </div>
      )}
    </div>
  );
}
