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
  fileOpsPending,
  fileOpCancel,
  fileOpDropPending,
  fileOpPause,
  fileOpResume,
  filePreview,
  fileRenameEntry,
  fileRemoteDrivers,
  fileRemotePresets,
  fileRemoteProfiles,
  xferStatus,
  parseAppError,
  endpointText,
  dispatchNameFix,
  type ConflictItemDto,
  type ConflictPolicyDto,
  type FileEndpointDto,
  type FileEntryDto,
  type FileOpKind,
  type NameFixItemDto,
  type OpProgressDto,
  type PendingOpDto,
  type PreviewDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import NameFixDialog from "./NameFixDialog";
import { isFileSubPanel, useSession } from "../../stores/session";
import { parse_magic_target } from "./magicTarget";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import BatchSection, { zipTarget } from "./BatchSection";
import ConnectionsSection from "./ConnectionsSection";
import FileSettingsSection from "./FileSettingsSection";
import NetdiskSection from "./NetdiskSection";
import SearchSection from "./SearchSection";

/**
 * 文件与存储面板（docs/impl/05 F，M6 v1；T-B7-27 七档分派）：
 * ① 盘符/面包屑/目录列表导航 ② 选中复制/移动/删除入队（Ask 冲突预扫描）
 * ③ operation.progress 事件驱动的操作队列 ④ 新建目录 ⑤ 右侧预览分栏（F4 四形态）。
 * 删除与「全部覆盖」属破坏性操作，一律经 confirmAction 二次确认（审查 D-18）。
 * 七档（panels/04 §2）：文件=本体内导航/操作/表格/预览；搜索·批量工具·网盘·设置
 * 拆出同名 Section；传输=transfersArm；连接=ConnectionsSection。**只挪分派不重写**
 * （T-B5-8/T-B6-10 同纪律）：跨档共享的 cwd/selected/dstInput/预览状态留在本组件
 * （FilePanel 恒挂载，切档不丢选中集与位置）。
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

export default function FilePanel() {
  const styles = useStyles();
  // T-B6-10 立三档，T-B7-27 扩七档（第四枚分键 fileSubPanel）：文件/传输/搜索/批量工具/
  // 远程连接/网盘/设置各占一档，既有代码原样进各档 Section（只挪分派不重写，T-B5-8 同纪律）；
  // 野值确定性回落 browse（面板侧收窄，store 侧已拒落）
  const storedSub = useSession((s) => s.fileSubPanel);
  const sub = isFileSubPanel(storedSub) ? storedSub : "browse";
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
    dst: FileEndpointDto;
    srcs: string[];
  } | null>(null);
  // T-B7-26 名闸 Ask 臂：预览行 + 挂起重投的 spec（确认后带 name_fix 覆盖重投）
  const [nameFix, setNameFix] = useState<{ items: NameFixItemDto[]; srcs: string[] } | null>(
    null
  );
  const [ops, setOps] = useState<OpProgressDto[]>([]);
  const [mkdirName, setMkdirName] = useState("");
  const [dstInput, setDstInput] = useState("");
  const [drives, setDrives] = useState<[string, string][]>([]);
  const opsRef = useRef<Map<string, OpProgressDto>>(new Map());
  const resumeFocusRef = useRef<string | null>(null);
  // ---- T-B1-4 预览（搜索面随 T-B7-27 拆入 SearchSection，其状态归该档自持）----
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<PreviewDto | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewErr, setPreviewErr] = useState<string | null>(null);
  const previewSeq = useRef(0);
  // ---- T-B1-5 等待队列（批量重命名工作台随 T-B7-27 拆入 BatchSection）----
  const [pending, setPending] = useState<PendingOpDto[]>([]);

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
          /* T-B6-9 收口后焦点行恒在 latest（resume 移旧行、prune 只裁终态）；
             此处仅兜"入队节拍晚一拍"的窗口，查不到即不追加，不谎报 */
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

  // 魔术栏消费点（magicTarget.ts 的 T-B6-10 交付形状由本行接线）：drive 与
  // remote 的解析分派只走 parse_magic_target 一处；"认不出的输入当本地路径
  // 放行"没有算式——null 就是点名不识别，绝不"当远端主机名试试"。
  const resolveMagicDst = async (
    text: string,
  ): Promise<{ ok: true; dst: FileEndpointDto } | { ok: false }> => {
    let presets: Awaited<ReturnType<typeof fileRemotePresets>> = [];
    try {
      presets = await fileRemotePresets();
    } catch {
      // 预设不可达按无预设处理：drive/盘符形态不受牵连（正对照在既有用例里）
    }
    const t = parse_magic_target(text, presets);
    if (!t) {
      setError(
        "无法识别为路径或连接地址（认盘符/UNC/目录路径，或 协议://主机/路径、预设主机直呼）",
      );
      return { ok: false };
    }
    if (t.kind === "drive") return { ok: true, dst: text };
    const m = /^([a-z_+]+):\/\/([^/]+)(\/.*)?$/.exec(t.value);
    if (!m) {
      setError("无法识别为路径或连接地址");
      return { ok: false };
    }
    const [, proto, host, rest] = m;
    try {
      const profiles = await fileRemoteProfiles();
      const cands = profiles.filter((p) => p.protocol === proto && p.host === host);
      if (cands.length === 0) {
        setError(`站点 ${host}（${proto}）没有建档的连接档案：先在「远程连接」新建`);
        return { ok: false };
      }
      if (cands.length > 1) {
        setError(
          `站点 ${host} 命中 ${cands.length} 枚连接档案（${cands
            .map((p) => p.label)
            .join("、")}）：请在连接列表里操作，魔术栏不替你选`,
        );
        return { ok: false };
      }
      const drivers = await fileRemoteDrivers();
      if (!drivers.some((d) => d.driver_id === cands[0].id)) {
        setError(`站点 ${host} 的档案「${cands[0].label}」未连接：请先连接（不代你自动重连）`);
        return { ok: false };
      }
      return { ok: true, dst: { driver_id: cands[0].id, path: rest ?? "/" } };
    } catch (e) {
      applyError(e, "魔术栏目标解析失败");
      return { ok: false };
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
    // copy/move 的目标即用户输入（T-B6-11 起经魔术栏分派，可为远端端点）；
    // delete/compress/extract 由调用方算好经 dst 传入
    let target: FileEndpointDto = dst;
    if (kind === "copy" || kind === "move") {
      const r = await resolveMagicDst(dstInput.trim());
      if (!r.ok) return;
      target = r.dst;
    }
    try {
      const res = await fileEnqueue({
        kind,
        srcs,
        dst: target,
        policy,
        recycle: kind === "delete",
      });
      await dispatchEnqueue(res, { kind, dst: target, srcs });
    } catch (e) {
      applyError(e, "操作入队失败");
    }
  };

  // T-B7-26 入队回执三臂分派（判据纯函数 = client.ts dispatchNameFix）：
  // AutoRename 臂 toast 逐行复述 原名→新名——静默改名与传败同罪，
  // 改了什么必须看得见
  const dispatchEnqueue = (
    res: Awaited<ReturnType<typeof fileEnqueue>>,
    spec: { kind: FileOpKind; dst: FileEndpointDto; srcs: string[] }
  ) => {
    setError(null);
    const d = dispatchNameFix(res);
    switch (d.kind) {
      case "nameFixPreview":
        setPendingSpec(spec);
        setNameFix({ items: d.items, srcs: spec.srcs });
        break;
      case "nameFixRenamed":
        notify(
          "info",
          "远端名按冲突字符映射改名后入队",
          d.items.map((i) => `${i.name} → ${i.suggested ?? "？"}`).join("\n")
        );
        void refreshOps();
        break;
      case "conflicts":
        setConflicts(d.items);
        setPendingSpec(spec);
        break;
      case "done":
        void refreshOps();
        break;
    }
  };

  const confirmNameFix = async () => {
    if (!nameFix || !pendingSpec) return;
    const spec = pendingSpec;
    setNameFix(null);
    try {
      const res = await fileEnqueue({
        kind: spec.kind,
        srcs: spec.srcs,
        dst: spec.dst,
        policy: "ask",
        name_fix: "auto_rename",
      });
      // 逐请求覆盖恒 auto_rename ⇒ 不会再回预览；行复述走 toast 臂
      await dispatchEnqueue(res, spec);
    } catch (e) {
      applyError(e, "改名重投失败");
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
            `目标目录：${endpointText(spec.dst)}`,
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

  const activeOps = ops.filter((p) =>
    ["queued", "running", "paused"].includes(p.state),
  );
  const finishedOps = ops.filter((p) =>
    ["done", "failed", "canceled"].includes(p.state),
  );

  // transfers 档（T-B6-10）：操作队列 + 崩溃恢复记录 + 终态摘要三块自 browse 档
  // 原样挪来（逐字未改）；该档首屏不空由诚实空态承担（D-18）
  const transfersArm = (
    <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
      {/* 操作队列（F2：进度 + 暂停/恢复/取消） */}
      {activeOps.length > 0 && (
        <div className={styles.queue}>
          {activeOps.map((p) => {
            const ratio = p.bytes_total > 0 ? p.bytes_done / p.bytes_total : 0;
            return (
              <div key={p.op_id} data-op-id={p.op_id} className={styles.opRow}>
                <Badge appearance="outline">{OP_KIND_LABEL[p.kind] ?? p.kind}</Badge>
                {/* 方向徽标只贴跨边界行（T-B6-9）：本地复制不显示方向——防
                    "处处贴方向"噪音；值来自 direction 唯一派生，面板不自判 */}
                {p.direction !== "local" && (
                  <Badge appearance="outline" color="important">
                    {p.direction === "upload" ? "上传" : "下载"}
                  </Badge>
                )}
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
                {p.srcs[0] ? endpointText(p.srcs[0]).split(/[\\/]/).pop() : "（无源）"}
                {p.srcs.length > 1 ? ` 等 ${p.srcs.length} 项` : ""}
                {` → ${endpointText(p.dst)} · 断点文件 ${p.file_index} · ${fmtTime(p.created_ms)}`}
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

      {activeOps.length === 0 && pending.length === 0 && finishedOps.length === 0 && (
        <EmptyState text="当前没有进行中或近期的传输：复制/移动/删除入队后在这里跟进" />
      )}
    </div>
  );

  // 预览分栏（F4）：browse 与 search 两档共用的同一份预览状态（openPreview 留本组件，
  // 命中点击与表格双击走同一口）
  const previewPane = (
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
              <img src={preview.data_url} alt={`${previewPath} 预览`} className={styles.previewImg} />
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
  );

  return (
    <div className={styles.root}>
    {sub === "browse" && (
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
        {/* T-B7-27 七档分派：搜索框/批量工具/三枚徽标随各自档挪出本导航栏，判据未动 */}
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
        <Button
          size="small"
          appearance="secondary"
          disabled={selected.size !== 1}
          onClick={() => void doRenameOne()}
        >
          重命名
        </Button>
        <span style={{ flex: 1 }} />
        <InlineError text={error} />
      </div>

      {/* 冲突决议（F3：Ask 预扫描，"应用到全部"） */}
      {conflicts && (
        <div className={styles.conflictBox}>
          <Text weight="semibold" size={300}>
            {conflicts.length} 个同名冲突（目标：{pendingSpec ? endpointText(pendingSpec.dst) : ""}）
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

      {/* 名闸 Ask 预览（T-B7-26）：按次挂载——items 随裁决整批换新，常驻挂载
          会把旧预览行定格在对话框里（ConnectDialog 补记③同谱） */}
      {nameFix && (
        <NameFixDialog
          open
          items={nameFix.items}
          onConfirm={confirmNameFix}
          onCancel={() => setNameFix(null)}
        />
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
        {previewPane}
      </div>
    )}
    {sub === "search" && (
      <div className={styles.split}>
        <div className={styles.leftCol}>
          <SearchSection onOpenPreview={(p) => void openPreview(p)} />
        </div>
        {previewPane}
      </div>
    )}
    {sub === "batch" && (
      <BatchSection
        cwd={cwd}
        entries={entries}
        selected={selected}
        error={error}
        onReload={(p) => void loadDir(p)}
        onCompress={() => void doCompress()}
        onExtract={() => void doExtract()}
      />
    )}
    {sub === "transfers" && transfersArm}
    {sub === "connections" && <ConnectionsSection />}
    {sub === "netdisk" && <NetdiskSection />}
    {sub === "settings" && <FileSettingsSection />}
    </div>
  );
}
