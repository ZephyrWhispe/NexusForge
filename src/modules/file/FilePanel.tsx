import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
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
  fileOpsPending,
  fileOpCancel,
  fileOpDropPending,
  fileOpPause,
  fileOpResume,
  filePreview,
  fileRenameApply,
  fileRenameEntry,
  fileRenamePlan,
  fileSearch,
  xferStatus,
  parseAppError,
  type ConflictItemDto,
  type ConflictPolicyDto,
  type FileEntryDto,
  type FileOpKind,
  type OpProgressDto,
  type PendingOpDto,
  type PreviewDto,
  type RenameCaseDto,
  type RenamePlanDto,
  type SearchResultDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 文件与存储面板（docs/impl/05 F，M6 v1）：
 * ① 盘符/面包屑/目录列表导航 ② 选中复制/移动/删除入队（Ask 冲突预扫描）
 * ③ operation.progress 事件驱动的操作队列 ④ 新建目录
 * ⑤ 全局搜索（F5：USN 优先，降级遍历必须显式标注）⑥ 右侧预览分栏（F4 四形态）。
 * 删除与「全部覆盖」属破坏性操作，一律经 confirmAction 二次确认（审查 D-18）。
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
  // T-B1-4 分栏：左列（导航/列表/队列）与右预览栏各自持有 overflow，互不挤压
  split: { flex: 1, minHeight: 0, minWidth: 0, display: "flex", gap: "12px" },
  leftCol: { flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "12px" },
  previewPane: {
    width: "380px",
    flexShrink: 0,
    minHeight: 0,
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground1,
    padding: "10px 12px",
    display: "flex",
    flexDirection: "column",
    gap: "8px",
  },
  previewImg: { maxWidth: "100%", borderRadius: tokens.borderRadiusMedium },
  pre: {
    margin: 0,
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    fontSize: tokens.fontSizeBase200,
    fontFamily: "Consolas, Menlo, monospace",
  },
  hitRow: { display: "flex", alignItems: "center", gap: "8px", minWidth: 0 },
  pathLine: {
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
    wordBreak: "break-all",
  },
  warn: { color: tokens.colorPaletteDarkOrangeForeground1, fontSize: tokens.fontSizeBase200 },
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
  rnField: { display: "flex", flexDirection: "column", gap: "2px" },
  rnPlanRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    minWidth: 0,
    fontSize: tokens.fontSizeBase200,
  },
  rnPath: { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: "180px" },
  bar: { flex: 1, minWidth: "120px" },
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

/** 确认框影响面点名（D-18）：取文件名并截断为"前 N 个 + 等 M 项" */
function namePreview(paths: string[], max = 3): string {
  const names = paths.map((p) => p.split(/[\\/]/).pop() || p);
  if (names.length === 0) return "（无）";
  const head = names.slice(0, max).join("、");
  return names.length > max ? `${head} 等 ${names.length} 项` : head;
}

const OP_KIND_LABEL: Record<string, string> = {
  copy: "复制",
  move: "移动",
  delete: "删除",
  compress: "压缩",
  extract: "解压",
};

/** 六态中文档（T-B6-7）：Badge 只报裸态名的旧形状在此换成受控词表，键=线上 snake_case */
const OP_STATE_LABEL: Record<string, string> = {
  queued: "排队中",
  running: "进行中",
  paused: "已暂停",
  done: "已完成",
  failed: "已失败",
  canceled: "已取消",
};

/**
 * 冲突原因前端推导（T-B1-5）：后端 conflict 是单一布尔、两种成因不可分辨
 * （rename.rs:141-149 目标已存在 || 计划内重复），故"表内重复"由计划内同名
 * 目标计数（>1）判出，其余冲突如实标"目标已存在"。键=from 路径。
 */
export function renameConflictReasons(plans: RenamePlanDto[]): Map<string, string> {
  const targetCount = new Map<string, number>();
  for (const p of plans) {
    if (p.from === p.to) continue;
    const k = p.to.toLowerCase();
    targetCount.set(k, (targetCount.get(k) ?? 0) + 1);
  }
  const out = new Map<string, string>();
  for (const p of plans) {
    if (!p.conflict) continue;
    out.set(p.from, (targetCount.get(p.to.toLowerCase()) ?? 0) > 1 ? "表内重复" : "目标已存在");
  }
  return out;
}

/**
 * zip 目标路径推导：目标输入留空 → 当前目录\<主名>.zip；以 \ 结尾或裸盘符
 * 视作目录拼自动名；其余按完整 zip 文件路径原样使用（run_compress 的 dst 是
 * zip 文件本体而非目录，ops.rs:916）。
 */
export function zipTarget(cwd: string, dstInput: string, stem: string): string {
  const d = dstInput.trim();
  const name = `${stem}.zip`;
  const base = cwd.replace(/\\+$/, "");
  if (!d) return `${base}\\${name}`;
  if (/\\$/.test(d)) return `${d}${name}`;
  if (/^[A-Za-z]:$/.test(d)) return `${d}\\${name}`;
  return d;
}

export default function FilePanel() {
  const styles = useStyles();
  const [cwd, setCwd] = useState<string | null>(null);
  const [crumbs, setCrumbs] = useState<[string, string][]>([]);
  const [entries, setEntries] = useState<FileEntryDto[]>([]);
  // 首轮目录读取是否落定（成功或失败）：未落定前列表渲染加载态而非"目录为空"（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [conflicts, setConflicts] = useState<ConflictItemDto[] | null>(null);
  // 冲突挂起的操作：srcs 一并暂存，避免决议时读到已变动的当前选中集（确认框按此计数）
  const [pendingSpec, setPendingSpec] = useState<{
    kind: FileOpKind;
    dst: string;
    srcs: string[];
  } | null>(null);
  const [ops, setOps] = useState<OpProgressDto[]>([]);
  const [mkdirName, setMkdirName] = useState("");
  const [dstInput, setDstInput] = useState("");
  const [drives, setDrives] = useState<[string, string][]>([]);
  const opsRef = useRef<Map<string, OpProgressDto>>(new Map());
  const resumeFocusRef = useRef<string | null>(null);
  // ---- T-B1-4 搜索 + 预览 ----
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchRes, setSearchRes] = useState<SearchResultDto | null>(null);
  const searchSeq = useRef(0);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewDto | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewErr, setPreviewErr] = useState<string | null>(null);
  const previewSeq = useRef(0);
  // ---- T-B1-5 等待队列 + 批量重命名 ----
  const [pending, setPending] = useState<PendingOpDto[]>([]);
  const [rnOpen, setRnOpen] = useState(false);
  const [rnTemplate, setRnTemplate] = useState("{name}{ext}");
  const [rnRegex, setRnRegex] = useState("");
  const [rnReplacement, setRnReplacement] = useState("");
  const [rnCase, setRnCase] = useState<RenameCaseDto>("none");
  const [rnStart, setRnStart] = useState("1");
  const [rnBusy, setRnBusy] = useState(false);
  const [rnPlans, setRnPlans] = useState<RenamePlanDto[] | null>(null);
  const [rnErr, setRnErr] = useState<string | null>(null);
  const [rnChecked, setRnChecked] = useState<Set<string>>(new Set());

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
      } finally {
        setLoaded(true);
      }
    },
    [applyError],
  );

  // 初始定位到主目录；加载盘符下拉
  useEffect(() => {
    void loadDir("C:\\").catch((e) => reportError(e, { context: "文件面板初始加载异常", toast: false }));
    fileDrives()
      .then((ds) => setDrives(ds.map((d) => [d.letter, String(d.path)] as [string, string])))
      .catch((e) =>
        reportError(e, { context: "盘符列表加载失败", dedupeKey: "file-drives", toast: false }),
      );
  }, [loadDir]);

  const refreshOps = useCallback(async () => {
    try {
      let list = await fileOpsActive();
      // 续传焦点（T-B6-7）：resume 产出的新 op_id 即该行新身份；全表按事件节拍
      // 重取可能晚一拍，用 xfer_status 定点兜底，"哪条是续上的"当场可见
      const focus = resumeFocusRef.current;
      if (focus && !list.some((p) => p.op_id === focus)) {
        try {
          const st: OpProgressDto = await xferStatus(focus);
          list = [...list, st];
        } catch {
          /* 焦点行已不在 latest（终态幽灵行），清理收口归 T-B6-9，不在此谎报 */
        }
      }
      for (const p of list) opsRef.current.set(p.op_id, p);
      // 只保留近端（Done/Failed 保留至下一次刷新窗口）
      setOps(list.slice(-8));
    } catch (e) {
      reportError(e, { context: "文件操作进度刷新失败", dedupeKey: "file-ops-refresh", toast: false });
    }
  }, []);

  // 等待中（崩溃恢复的 pending_ops 记录，F2/docs/impl-01 S6.5）
  const loadPending = useCallback(async () => {
    try {
      setPending(await fileOpsPending());
    } catch (e) {
      reportError(e, { context: "等待队列刷新失败", dedupeKey: "file-ops-pending", toast: false });
    }
  }, []);

  useEffect(() => {
    void loadPending();
    // 挂载即拉一次在途传输：只靠事件刷新的旧形在"打开面板时已有传输"下整段隐形
    void refreshOps();
  }, [loadPending, refreshOps]);

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
          // pending 行只在完成/失败/取消时被后端消费，终态事件顺带刷新等待队列
          if (topic === "operation.done" || topic === "operation.failed") {
            void loadPending();
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
      .catch((e) => reportError(e, { context: "文件面板事件监听注册失败", toast: false }));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [refreshOps, loadPending]);

  const toggleSelect = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const openPreview = async (path: string) => {
    const seq = ++previewSeq.current;
    // 切换目标即清空旧内容：加载期间预览栏不得残留上一文件的形态（防说谎分栏）
    setPreviewPath(path);
    setPreview(null);
    setPreviewErr(null);
    setPreviewLoading(true);
    try {
      const dto = await filePreview(path);
      if (seq !== previewSeq.current) return;
      setPreview(dto);
    } catch (e) {
      if (seq !== previewSeq.current) return;
      const err = parseAppError(e);
      setPreviewErr(err ? `${err.data.code}: ${err.data.message}` : "预览失败");
    } finally {
      if (seq === previewSeq.current) setPreviewLoading(false);
    }
  };

  const openEntry = (e: FileEntryDto) => {
    if (e.is_dir) void loadDir(e.path);
    else void openPreview(e.path);
  };

  // 全局搜索（F5）：limit 取默认档位 50；root 传 null → 降级遍历走后端默认用户主目录，
  // 不随当前 cwd（否则站在 C:\ 会把降级遍历扩成整盘扫描）。
  const runSearch = async () => {
    const q = query.trim();
    if (!q) {
      setSearchRes(null);
      return;
    }
    const seq = ++searchSeq.current;
    setSearching(true);
    setSearchRes(null);
    try {
      const res = await fileSearch(q, 50, null);
      if (seq === searchSeq.current) setSearchRes(res);
    } catch (e) {
      if (seq === searchSeq.current) applyError(e, "搜索失败");
    } finally {
      if (seq === searchSeq.current) setSearching(false);
    }
  };

  const onQueryChange = (v: string) => {
    setQuery(v);
    if (!v.trim()) {
      // 清空即作废在途请求并撤下结果区（负例判据：空查询不残留旧命中）
      searchSeq.current += 1;
      setSearching(false);
      setSearchRes(null);
    }
  };

  const enqueue = async (
    kind: FileOpKind,
    dst = "",
    policy: ConflictPolicyDto = "ask",
  ) => {
    if (!cwd || selected.size === 0) return;
    if ((kind === "copy" || kind === "move") && !dstInput.trim()) {
      setError("请在「目标目录」输入框填写目标路径");
      return;
    }
    const srcs = [...selected];
    // 删除入队（D-18）：单击即删改为全局确认框，影响面按选中数/目录/文件名点名。
    // 本面板 delete 固定 recycle=true（见下方 fileEnqueue 的 recycle 实参），故承诺"移入回收站"。
    if (kind === "delete") {
      const dirCount = entries.filter((e) => selected.has(e.path) && e.is_dir).length;
      if (
        !(await confirmAction({
          title: "删除文件",
          impact: [
            `将删除 ${srcs.length} 个条目（${dirCount} 个目录 / ${srcs.length - dirCount} 个文件）`,
            `所在目录：${cwd}`,
            `包含：${namePreview(srcs)}`,
          ],
          detail: "条目移入系统回收站（目录连同其内容整体移入），可从回收站还原；本面板不提供永久删除。",
          confirmLabel: "移入回收站",
        }))
      )
        return;
    }
    // copy/move 的目标即用户输入；delete/compress/extract 由调用方算好经 dst 传入
    const target = kind === "copy" || kind === "move" ? dstInput.trim() : dst;
    try {
      const res = await fileEnqueue({
        kind,
        srcs,
        dst: target,
        policy,
        recycle: kind === "delete",
      });
      if (res.conflicts.length > 0 && res.op_id === null) {
        setConflicts(res.conflicts);
        setPendingSpec({ kind, dst: target, srcs });
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
    const spec = pendingSpec;
    const items = conflicts ?? [];
    // 覆盖决议（D-18）：目标同名文件被原地重写且不进回收站 → 确认后才入队；
    // 取消则保留冲突框，可改选「重命名保留两者」或「全部跳过」。
    if (policy === "overwrite") {
      if (
        !(await confirmAction({
          title: "覆盖同名文件",
          impact: [
            `将覆盖 ${items.length} 个已存在的目标文件`,
            `目标目录：${spec.dst}`,
            `被覆盖：${namePreview(items.map((c) => c.dst))}`,
          ],
          detail: "覆盖为原地重写，被覆盖的原件不进回收站且无法找回；需保留两侧请改选「重命名保留两者」。",
          confirmLabel: "全部覆盖",
        }))
      )
        return;
    }
    setConflicts(null);
    setPendingSpec(null);
    try {
      await fileEnqueue({ kind: spec.kind, srcs: spec.srcs, dst: spec.dst, policy });
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
      else {
        // 消费 ResumeDto（T-B6-7）：新 op_id 作行的新身份，断点链经 resumed_from 指回旧行
        const r = await fileOpResume(p.op_id);
        resumeFocusRef.current = r.op_id;
      }
      void refreshOps();
    } catch (e) {
      applyError(e, "操作控制失败");
    }
  };

  const dropPending = async (p: PendingOpDto) => {
    try {
      const gone = await fileOpDropPending(p.op_id);
      if (gone) notify("success", "已丢弃等待记录", p.op_id);
      else notify("warn", "该等待记录已不存在", "可能刚被启动崩溃恢复消费");
      void loadPending();
    } catch (e) {
      applyError(e, "丢弃等待记录失败");
    }
  };

  // ---- T-B1-5 压缩 / 解压 / 行内重命名 ----
  const doCompress = async () => {
    if (!cwd || selected.size === 0) return;
    const srcs = [...selected];
    const base = srcs[0].split(/[\\/]/).filter(Boolean).pop() ?? "archive";
    const stem = srcs.length === 1 ? base.replace(/\.[^.]+$/, "") || "archive" : "打包";
    const target = zipTarget(cwd, dstInput, stem);
    // 压缩不参与冲突预扫描（service.rs:117 仅 Copy|Move），dst 已存在会被整体重写 → D-18 确认点名
    if (
      !(await confirmAction({
        title: "压缩为 zip",
        impact: [`目标文件：${target}`, `共 ${srcs.length} 个条目：${namePreview(srcs)}`],
        detail:
          "目标 zip 若已存在会被整体重写（覆盖内容不进回收站）。压缩/移动的重名冲突预扫描互不适用，入队前请自行核对目标路径。",
        confirmLabel: "入队压缩",
      }))
    )
      return;
    void enqueue("compress", target);
  };

  const doExtract = async () => {
    if (!cwd || selected.size !== 1) {
      setError("解压需且仅需选中 1 个压缩包（后端只取所选首个源）");
      return;
    }
    const zip = [...selected][0];
    if (!/\.zip$/i.test(zip)) {
      setError("解压目前仅支持 zip 格式");
      return;
    }
    const dst = dstInput.trim() || cwd;
    if (
      !(await confirmAction({
        title: "解压",
        impact: [`压缩包：${zip}`, `目标目录：${dst}`],
        detail:
          "包内条目以压缩包名目录归组释放；目标已有同名文件时自动重命名保留两者（后端的解压安全默认，不覆盖既有文件）。",
        confirmLabel: "入队解压",
      }))
    )
      return;
    void enqueue("extract", dst, "rename");
  };

  // 行内单项重命名：复用「目标目录」输入框——纯名称视为当前目录内改名，含 \ 或盘符开头按完整路径
  const doRenameOne = async () => {
    if (!cwd || selected.size !== 1) return;
    const from = [...selected][0];
    const v = dstInput.trim();
    if (!v) {
      setError("请在「目标目录」输入框填写新名称或完整路径");
      return;
    }
    const to = /^[A-Za-z]:/.test(v) || v.includes("\\") ? v : `${cwd.replace(/\\+$/, "")}\\${v}`;
    try {
      await fileRenameEntry(from, to);
      notify(
        "success",
        "已重命名",
        `${from.split(/[\\/]/).pop()} → ${to.split(/[\\/]/).pop()}`,
      );
      void loadDir(cwd);
    } catch (e) {
      applyError(e, "重命名失败");
    }
  };

  // ---- 批量重命名 Dialog（F7：预览→勾选→应用，冲突条目后端兜底跳过）----
  const openRename = () => {
    setRnPlans(null);
    setRnErr(null);
    setRnOpen(true);
  };

  const doRenamePlan = async () => {
    if (!cwd) return;
    // 只交文件主名：显式 names 不做目录过滤（rename.rs:78-86），目录必须由前端挡下
    const names = entries.filter((e) => selected.has(e.path) && !e.is_dir).map((e) => e.name);
    if (selected.size > 0 && names.length === 0) {
      setRnErr("所选条目全是目录：仅文件参与批量重命名");
      return;
    }
    const parsed = Number.parseInt(rnStart, 10);
    setRnBusy(true);
    setRnErr(null);
    setRnPlans(null);
    try {
      const plans = await fileRenamePlan(cwd, names, {
        template: rnTemplate,
        regex: rnRegex.trim() ? rnRegex : null,
        replacement: rnReplacement,
        case: rnCase,
        start: Number.isNaN(parsed) || parsed < 0 ? 1 : parsed,
      });
      setRnPlans(plans);
      // 默认只勾非冲突、非 no-op 条目（服务端对冲突条目也会再次跳过，双保险）
      setRnChecked(new Set(plans.filter((p) => !p.conflict && p.from !== p.to).map((p) => p.from)));
    } catch (e) {
      const err = parseAppError(e);
      setRnErr(err ? `${err.data.code}: ${err.data.message}` : "生成重命名预览失败");
    } finally {
      setRnBusy(false);
    }
  };

  const doRenameApply = async () => {
    if (!cwd || !rnPlans) return;
    const checked = rnPlans.filter((p) => rnChecked.has(p.from));
    if (checked.length === 0) {
      setRnErr("未勾选任何可执行条目");
      return;
    }
    try {
      const n = await fileRenameApply(checked);
      notify("success", "批量重命名完成", `已重命名 ${n} 项；冲突与未勾选条目未执行。`);
      setRnOpen(false);
      setRnPlans(null);
      void loadDir(cwd);
    } catch (e) {
      const err = parseAppError(e);
      setRnErr(err ? `${err.data.code}: ${err.data.message}` : "应用重命名失败");
    }
  };

  const toggleRn = (from: string) => {
    setRnChecked((prev) => {
      const next = new Set(prev);
      if (next.has(from)) next.delete(from);
      else next.add(from);
      return next;
    });
  };

  const rnReasons = rnPlans ? renameConflictReasons(rnPlans) : null;

  const activeOps = ops.filter((p) =>
    ["queued", "running", "paused"].includes(p.state),
  );
  const finishedOps = ops.filter((p) =>
    ["done", "failed", "canceled"].includes(p.state),
  );

  return (
    <div className={styles.root}>
      <div className={styles.split}>
        <div className={styles.leftCol}>
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
        <DeferredBadge label="网盘" decisionRef="B6" />
        <Input
          size="small"
          placeholder="搜索文件名（全局）"
          value={query}
          onChange={(_, d) => onQueryChange(d.value)}
          onKeyDown={(ev) => ev.key === "Enter" && void runSearch()}
          aria-label="全局搜索关键词"
          style={{ maxWidth: "180px" }}
        />
        <Button size="small" onClick={() => void runSearch()}>
          搜索
        </Button>
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
        <Button size="small" onClick={openRename}>
          批量重命名
        </Button>
      </div>

      {/* 搜索结果条（F5）：加载/空查询不残留旧命中；降级必须显式标注 */}
      {searching && (
        <div className={styles.queue} role="status">
          <Text className={styles.muted}>正在全局搜索…</Text>
        </div>
      )}
      {searchRes && !searching && (
        <div className={styles.queue}>
          {searchRes.degraded && (
            <Text className={styles.warn}>索引降级：本次为目录遍历（深度≤6）</Text>
          )}
          {searchRes.hits.length === 0 && (
            <Text className={styles.muted}>
              没有名称匹配「{query.trim()}」的命中
              {searchRes.degraded ? "（降级遍历仅覆盖用户主目录）" : ""}
            </Text>
          )}
          {searchRes.hits.slice(0, 12).map((h) => (
            <div key={h.path} className={styles.hitRow}>
              <Button
                appearance="subtle"
                size="small"
                className={styles.crumbBtn}
                title={h.path}
                onClick={() => void openPreview(h.path)}
              >
                {h.path}
              </Button>
              <Badge appearance="outline">{h.score}</Badge>
            </div>
          ))}
          {searchRes.hits.length > 12 && (
            <Text className={styles.muted}>共 {searchRes.hits.length} 条，仅显示前 12 条</Text>
          )}
        </div>
      )}

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
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size !== 1}
          onClick={() => void doRenameOne()}
        >
          重命名
        </Button>
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size === 0}
          onClick={() => void doCompress()}
        >
          压缩为 zip
        </Button>
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size !== 1}
          onClick={() => void doExtract()}
        >
          解压
        </Button>
        <span style={{ flex: 1 }} />
        <InlineError text={error} />
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
              <div key={p.op_id} data-op-id={p.op_id} className={styles.opRow}>
                <Badge appearance="outline">{OP_KIND_LABEL[p.kind] ?? p.kind}</Badge>
                <ProgressBar className={styles.bar} value={Math.min(1, Math.max(0, ratio))} />
                <span className={styles.muted}>
                  {fmtSize(p.bytes_done)} / {fmtSize(p.bytes_total)} · {p.files_done}/
                  {p.files_total}
                  {p.current ? ` · ${p.current}` : ""}
                  {` · ${OP_STATE_LABEL[p.state] ?? p.state}`}
                  {p.resumed_from ? ` · 续自 ${p.resumed_from.slice(0, 8)}` : ""}
                  {/* 续传档位只报对端声明（resumable=null 即"未获事实源"，禁写"支持断点续传"） */}
                  {p.resumable === "range" ? " · 支持断点续传" : ""}
                  {p.error ? ` · ${p.error}` : ""}
                </span>
                {p.state === "running" || p.state === "queued" ? (
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

      {/* 等待中（F2 崩溃恢复记录：pending_ops 表，可丢弃） */}
      {pending.length > 0 && (
        <div className={styles.queue}>
          <Text weight="semibold" size={200}>
            等待中（未完成操作的崩溃恢复记录）
          </Text>
          {pending.map((p) => (
            <div key={p.op_id} className={styles.opRow}>
              <Badge appearance="outline">{OP_KIND_LABEL[p.kind] ?? p.kind}</Badge>
              <Text size={200} className={styles.muted}>
                {p.srcs[0]?.split(/[\\/]/).pop() ?? "（无源）"}
                {p.srcs.length > 1 ? ` 等 ${p.srcs.length} 项` : ""}
                {` → ${p.dst} · 断点文件 ${p.file_index} · ${fmtTime(p.created_ms)}`}
              </Text>
              <span style={{ flex: 1 }} />
              <Button size="small" onClick={() => void dropPending(p)}>
                丢弃
              </Button>
            </div>
          ))}
          <Text size={200} className={styles.muted}>
            应用重启时以上条目会被自动断点续传；「丢弃」仅删除恢复记录，不影响已落盘文件。
          </Text>
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
                  <EmptyState
                    text={
                      cwd
                        ? "该目录没有可见条目：双击条目进入子目录，或用上方面包屑/盘符切换位置"
                        : "选择盘符或双击目录即可浏览"
                    }
                    loading={!loaded}
                  />
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>
        </div>

        {/* 预览分栏（F4：文本/图片/系统缩略图/不支持 四形态；限额服务端固定，UI 不承诺可调） */}
        <aside className={styles.previewPane} aria-label="文件预览">
          {!previewPath && (
            <EmptyState text="双击列表中的文件即可在此预览（搜索命中点击同效）" />
          )}
          {previewPath && (
            <>
              <Text className={styles.pathLine}>{previewPath}</Text>
              {previewLoading && <EmptyState text="预览加载中…" loading />}
              {!previewLoading && previewErr && <InlineError text={previewErr} />}
              {!previewLoading && preview?.kind === "text" && (
                <>
                  {preview.truncated && (
                    <Text className={styles.warn}>
                      内容较大：仅显示开头部分（截断限额由服务端固定）
                    </Text>
                  )}
                  <pre className={styles.pre}>{preview.content || "（文件内容为空）"}</pre>
                </>
              )}
              {!previewLoading && (preview?.kind === "image" || preview?.kind === "shell") && (
                <>
                  <img
                    src={preview.data_url}
                    alt={`${previewPath} 预览`}
                    className={styles.previewImg}
                  />
                  <Text className={styles.muted}>
                    {preview.kind === "image"
                      ? `原图 ${preview.width}×${preview.height}（超阈值时缩略显示）`
                      : `系统缩略图 ${preview.width}×${preview.height}`}
                  </Text>
                </>
              )}
              {!previewLoading && preview?.kind === "unsupported" && (
                <Text>无法预览：{preview.reason}</Text>
              )}
            </>
          )}
        </aside>
      </div>

      {/* 近期完成（终态摘要） */}
      {finishedOps.length > 0 && (
        <div className={styles.toolbar}>
          {finishedOps.map((p) => (
            <Badge key={p.op_id} data-op-id={p.op_id} appearance={p.state === "done" ? "filled" : "outline"} color={p.state === "done" ? "success" : "danger"}>
              {OP_KIND_LABEL[p.kind]} · {OP_STATE_LABEL[p.state] ?? p.state}
              {p.resumed_from ? ` · 续自 ${p.resumed_from.slice(0, 8)}` : ""}
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

      {/* 批量重命名 Dialog（F7：规则表单 → 预览表 → 勾选应用；冲突原因前端推导） */}
      <Dialog open={rnOpen} onOpenChange={(_, d) => !d.open && setRnOpen(false)}>
        <DialogSurface>
          <DialogBody>
            <DialogTitle>批量重命名</DialogTitle>
            <DialogContent>
              <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
                <Text size={200} className={styles.muted}>
                  仅文件，目录不参与。
                  {selected.size === 0
                    ? "未选中文件：将对当前目录全部文件生成计划。"
                    : `已选 ${entries.filter((e) => selected.has(e.path) && !e.is_dir).length} 个文件参与。`}
                </Text>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    模板（变量仅 {"{name}"} {"{ext}"} {"{n}"} {"{n:0N}"}，N≤10）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名模板"
                    value={rnTemplate}
                    onChange={(_, d) => setRnTemplate(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    正则（作用于主名，留空不处理）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名正则"
                    value={rnRegex}
                    onChange={(_, d) => setRnRegex(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    替换串（$1 组引用）
                  </Text>
                  <Input
                    size="small"
                    aria-label="重命名替换串"
                    value={rnReplacement}
                    onChange={(_, d) => setRnReplacement(d.value)}
                  />
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    大小写
                  </Text>
                  <Select
                    size="small"
                    aria-label="大小写转换"
                    value={rnCase}
                    onChange={(_, d) => setRnCase((d.value || "none") as RenameCaseDto)}
                  >
                    <option value="none">不转换</option>
                    <option value="lower">全部小写</option>
                    <option value="upper">全部大写</option>
                  </Select>
                </div>
                <div className={styles.rnField}>
                  <Text size={200} className={styles.muted}>
                    序号起始值
                  </Text>
                  <Input
                    size="small"
                    type="number"
                    aria-label="序号起始值"
                    value={rnStart}
                    onChange={(_, d) => setRnStart(d.value)}
                    style={{ maxWidth: "100px" }}
                  />
                </div>
                {rnErr && <InlineError text={rnErr} />}
                {rnBusy && (
                  <Text size={200} role="status" className={styles.muted}>
                    正在生成预览…
                  </Text>
                )}
                {rnPlans &&
                  rnPlans.map((p) => (
                    <div key={p.from} className={styles.rnPlanRow}>
                      <Checkbox
                        checked={rnChecked.has(p.from)}
                        onChange={() => toggleRn(p.from)}
                        aria-label={`选择 ${p.from}`}
                      />
                      <span className={styles.rnPath} title={p.from}>
                        {p.from.split(/[\\/]/).pop()}
                      </span>
                      <span>→</span>
                      <span className={styles.rnPath} title={p.to}>
                        {p.to.split(/[\\/]/).pop()}
                      </span>
                      {p.from === p.to && <Badge appearance="outline">不变</Badge>}
                      {rnReasons?.get(p.from) && (
                        <Badge appearance="tint" color="warning">
                          {rnReasons.get(p.from)}
                        </Badge>
                      )}
                    </div>
                  ))}
                {rnPlans && rnPlans.length === 0 && (
                  <Text size={200} className={styles.muted}>
                    计划为 0 条
                  </Text>
                )}
              </div>
            </DialogContent>
            <DialogActions>
              <Button onClick={() => void doRenamePlan()}>生成预览</Button>
              <Button
                appearance="primary"
                disabled={!rnPlans || rnBusy}
                onClick={() => void doRenameApply()}
              >
                应用（勾选 {rnChecked.size} 项）
              </Button>
              <Button appearance="subtle" onClick={() => setRnOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
