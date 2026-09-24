import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Checkbox,
  Spinner,
  Dialog,
  DialogSurface,
  DialogBody,
  DialogTitle,
  DialogContent,
  DialogActions,
} from "@fluentui/react-components";
import Section from "../../components/Section";
import Tabs from "../../components/Tabs";
import InlineError from "../../components/InlineError";
import { confirmAction } from "../../stores/confirm";
import { useSession } from "../../stores/session";
import {
  parseAppError,
  sysCleanExecute,
  sysCleanScan,
  sysCleanTargets,
  sysKill,
  sysMetricsHistory,
  sysPkgAction,
  sysPkgCmdPreview,
  sysPkgList,
  sysPkgSearch,
  sysPkgSources,
  sysProcesses,
  winopsApply,
  winopsCatalog,
  winopsRollback,
  winopsScan,
  winopsAuditExport,
  type CleanScanItemDto,
  type CleanTargetDto,
  type MetricsPointDto,
  type PkgEntryDto,
  type PkgSearchRowDto,
  type PkgSourceDto,
  type ProcessRowDto,
  type WinopsActionDto,
  type WinopsScanItemDto,
  type WinopsTweakDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";

/**
 * 系统管理面板（docs/impl/06 SY1–SY4，M12 v1）：
 * - SY4 监控：CPU/内存/网络 1s 采样折线（SVG 自绘，300 点缓冲；uPlot 偏离为减依赖，可后换）
 * - SY3 清理：扫描（24h 白名单）→ 勾选 → 执行（回收站可恢复）
 * - SY1/SY2 包管理：winget/scoop/choco 源探测 + 合并清单 + 变更（确切命令行确认）
 * - T-B1-9：清理页扫描前即呈现 sys_clean_targets 静态清单（dir/exts/optional 可见、
 *   safe_default 只做「推荐」角标不自动勾选），勾选态持久化在 session store；
 *   调整页「浏览目录」Dialog 按 category 分组展示全目录（含 maintenance 与生效方式）。
 * - T-B7-10 进程页红线：监控 Tab 内进程 Section（两拍差值 Top-N/排序头/搜索框），
 *   结束钮走「逐字复述进程名输入确认词 → confirmAction → sys_kill」双闸；
 *   保护名单与坏盘 fail-closed 在后端 sys-core ProcessTable，UI 闸不代替后端闸。
 * - T-B7-12 包管理在线搜索：搜索源面走 sys_pkg_search（argv 纯函数零 shell 拼接），
 *   结果表安装钮复用 doPkgAction 的确切命令行确认对话框（cmd_preview）；
 *   已装表每行「升级」钮走单包 upgrade 动作。
 */

/** 动作 type → 生效方式中文说明（前端纯函数；type 对照 winops.rs:52-105 serde tag） */
export function winopsEffectHint(actions: WinopsActionDto[]): string {
  const types = new Set(actions.map((a) => a.type));
  const HINTS: [string, string][] = [
    ["registry", "注册表写入（多数项需注销/重启后由系统读取）"],
    ["service", "服务启停/启动类型变更（需管理员）"],
    ["task", "计划任务启用/禁用（系统内置任务需管理员）"],
    ["file_clean", "文件清理（维护型动作，无「已应用」状态）"],
    ["appx_remove", "移除 Appx 包（全体用户预装移除需管理员）"],
    ["exec", "执行白名单系统命令"],
    ["restore_point", "创建系统还原点"],
    ["empty_working_set", "内存整理（清理各进程工作集）"],
    ["defender_realtime", "Defender 实时保护开关（篡改保护可能拦截）"],
    ["unsupported", "含需更高版本支持的动作类型"],
  ];
  const parts = HINTS.filter(([t]) => types.has(t)).map(([, hint]) => hint);
  return parts.length ? `生效方式：${parts.join(" · ")}` : "生效方式：见条目说明";
}

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
  grow: { flex: 1, minWidth: "120px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  chart: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
    maxHeight: "340px",
    overflowY: "auto",
  },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  log: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase100,
    whiteSpace: "pre-wrap",
    maxHeight: "180px",
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
  },
});

type TabId = "monitor" | "clean" | "pkg" | "tweaks";

/** SVG 折线（0-100 量程或自动） */
function Spark({ values, color, label, fmt }: { values: number[]; color: string; label: string; fmt?: (v: number) => string }) {
  const w = 520;
  const h = 90;
  const max = Math.max(1, ...values);
  const pts = values
    .map((v, i) => `${(i / Math.max(1, values.length - 1)) * w},${h - (v / (max * 1.15)) * (h - 6) - 3}`)
    .join(" ");
  const last = values.length ? values[values.length - 1] : 0;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <Text size={200} weight="semibold">
        {label}：<span style={{ color }}>{fmt ? fmt(last) : last.toFixed(1)}</span>
      </Text>
      <svg viewBox={`0 0 ${w} ${h}`} className="spark" style={{ width: "100%", height: h, background: tokens.colorNeutralBackground2, borderRadius: 6 }}>
        <polyline points={pts} fill="none" stroke={color} strokeWidth="1.6" />
      </svg>
    </div>
  );
}

export default function SysPanel() {
  const styles = useStyles();
  const [tab, setTab] = useState<TabId>("monitor");
  const [err, setErr] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const fail = useCallback((e: unknown) => setErr(parseAppError(e)?.data.message ?? String(e)), []);

  // ---- 监控 ----
  const [history, setHistory] = useState<MetricsPointDto[]>([]);
  const historyRef = useRef<MetricsPointDto[]>([]);

  // ---- 进程页（T-B7-10 红线：两拍差值 + 复述名输入确认词）----
  const [procs, setProcs] = useState<ProcessRowDto[]>([]);
  const [procSort, setProcSort] = useState<"cpu" | "mem" | "disk" | "name">("cpu");
  const [procQuery, setProcQuery] = useState("");
  const [procBusy, setProcBusy] = useState(false);
  /** 复述闸：非 null 时该行下方展开输入框，逐字复述进程名才可用确认钮 */
  const [killTarget, setKillTarget] = useState<ProcessRowDto | null>(null);
  const [killTyped, setKillTyped] = useState("");

  // ---- 清理 ----
  const [targets, setTargets] = useState<CleanTargetDto[]>([]);
  const [scan, setScan] = useState<CleanScanItemDto[]>([]);
  // 勾选态即 session store 持久键（T-B1-9），不再是面板内一次性 Set
  const sysCleanSelected = useSession((s) => s.sysCleanSelected);
  const setSysCleanSelected = useSession((s) => s.setSysCleanSelected);
  const selected = useMemo(() => new Set(sysCleanSelected), [sysCleanSelected]);
  const [scanning, setScanning] = useState(false);
  const [useRecycle, setUseRecycle] = useState(true);

  // ---- 包管理 ----
  const [sources, setSources] = useState<PkgSourceDto[]>([]);
  const [pkgs, setPkgs] = useState<PkgEntryDto[]>([]);
  const [pkgFilter, setPkgFilter] = useState("");
  const [pkgsLoading, setPkgsLoading] = useState(false);
  const [output, setOutput] = useState<string[]>([]);
  // T-B7-12 在线搜索：null=未搜索过（区分空结果态）
  const [onlineQuery, setOnlineQuery] = useState("");
  const [onlineResults, setOnlineResults] = useState<PkgSearchRowDto[] | null>(null);
  const [onlineBusy, setOnlineBusy] = useState(false);

  // ---- 系统调整（WinOps Tweak，M16 W1）----
  const [tweaks, setTweaks] = useState<WinopsScanItemDto[]>([]);
  const [tweaksLoading, setTweaksLoading] = useState(false);
  const [busyTweak, setBusyTweak] = useState<string | null>(null);
  // W4 回归检测（WUB 式防自愈）：模块 start 时后端比对，回归项在此黄条提示
  const [regressed, setRegressed] = useState<string[]>([]);
  // 浏览目录 Dialog（T-B1-9）：winops_catalog 全量静态目录，按 category 分组
  const [catalogOpen, setCatalogOpen] = useState(false);
  const [catalog, setCatalog] = useState<WinopsTweakDto[] | null>(null);

  // 事件驱动：sys.metrics 1s 推送
  useEffect(() => {
    void sysMetricsHistory()
      .then((h) => {
        historyRef.current = h;
        setHistory(h);
      })
      .catch((e) =>
        reportError(e, { context: "性能历史加载失败", dedupeKey: "sys-metrics-history", toast: false }),
      );
    if (!("__TAURI_INTERNALS__" in window)) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<{ topic: string; payload: MetricsPointDto | { line: string } }>("nf:event", (e) => {
          if (e.payload?.topic === "sys.metrics") {
            const p = e.payload.payload as unknown as MetricsPointDto;
            if (p && typeof p.ts_ms === "number") {
              historyRef.current = [...historyRef.current.slice(-299), p];
              setHistory(historyRef.current);
            }
          } else if (e.payload?.topic === "sys.pkg_line") {
            const pl = e.payload.payload as unknown as { line: string };
            if (pl?.line) setOutput((prev) => [...prev.slice(-200), pl.line]);
          } else if (e.payload?.topic === "sys.verify_result") {
            const vr = e.payload.payload as unknown as { regressed?: string[] };
            if (Array.isArray(vr?.regressed)) setRegressed(vr.regressed);
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
  }, []);

  // 静态清理目标清单（T-B1-9）：挂载即取——sys_clean_targets 是内置 4 条静态清单，
  // 扫描前后都可呈现，扫描前供勾选偏好落地（勾选持久化在 session store）。
  useEffect(() => {
    void sysCleanTargets()
      .then(setTargets)
      .catch((e) =>
        reportError(e, { context: "清理目标清单加载失败", dedupeKey: "sys-clean-targets", toast: false }),
      );
  }, []);

  const loadPkgs = useCallback(async () => {
    setPkgsLoading(true);
    setErr(null);
    try {
      setSources(await sysPkgSources());
      setPkgs(await sysPkgList());
    } catch (e) {
      fail(e);
    } finally {
      setPkgsLoading(false);
    }
  }, [fail]);

  const doScan = useCallback(async () => {
    setScanning(true);
    setErr(null);
    try {
      const items = await sysCleanScan();
      setScan(items);
      // safe_default 不自动勾选（勾选态是用户持久偏好）；仅剔除本次不可执行的 id
      const executable = new Set(
        items.filter((i) => !i.missing && i.files > 0).map((i) => i.target_id),
      );
      setSysCleanSelected(useSession.getState().sysCleanSelected.filter((id) => executable.has(id)));
    } catch (e) {
      fail(e);
    } finally {
      setScanning(false);
    }
  }, [fail, setSysCleanSelected]);

  const toggleTarget = useCallback(
    (id: string, on: boolean) => {
      const cur = useSession.getState().sysCleanSelected;
      setSysCleanSelected(on ? [...new Set([...cur, id])] : cur.filter((x) => x !== id));
    },
    [setSysCleanSelected],
  );

  const doExecute = useCallback(async () => {
    setErr(null);
    if (
      !(await confirmAction({
        title: "执行系统清理",
        impact: [
          `已勾选 ${selected.size} 项清理目标`,
          useRecycle ? "文件移入回收站，可从回收站恢复" : "未勾选回收站：文件将被直接删除，不可恢复",
        ],
        detail: "24h 内修改的文件已被白名单跳过。",
        danger: !useRecycle,
        confirmLabel: "开始清理",
      }))
    )
      return;
    try {
      const n = await sysCleanExecute([...selected], useRecycle);
      setMsg(`已清理 ${n} 个文件${useRecycle ? "（移入回收站，可恢复）" : ""}`);
      await doScan();
    } catch (e) {
      fail(e);
    }
  }, [selected, useRecycle, doScan, fail]);

  const doPkgAction = useCallback(
    async (source: string, action: string, id: string) => {
      setErr(null);
      try {
        const cmd = await sysPkgCmdPreview(source, action, id);
        const actionLabel =
          action === "install" ? "安装" : action === "uninstall" ? "卸载" : action === "upgrade" ? "单包升级" : "全部升级";
        if (
          !(await confirmAction({
            title: `包管理操作（${actionLabel}）`,
            impact: [`${source} · ${action}${id ? ` ${id}` : ""}`],
            command: cmd,
            danger: action !== "install",
          }))
        )
          return;
        setOutput([]);
        const lines = await sysPkgAction(source, action, id);
        setMsg(`${actionLabel} 完成（${lines.length} 行输出）`);
        if (action !== "upgrade_all") await loadPkgs();
      } catch (e) {
        fail(e);
      }
    },
    [loadPkgs, fail],
  );

  // T-B7-12 在线搜索：源取首个可用（winget 优先，与合并清单同谱）
  const doOnlineSearch = useCallback(async () => {
    const q = onlineQuery.trim();
    if (!q || onlineBusy) return;
    const src = sources.find((s) => s.available);
    if (!src) {
      setErr("无可用包管理器（winget / scoop / choco）");
      return;
    }
    setErr(null);
    setOnlineBusy(true);
    try {
      setOnlineResults(await sysPkgSearch(src.id, q));
    } catch (e) {
      fail(e);
    } finally {
      setOnlineBusy(false);
    }
  }, [onlineQuery, onlineBusy, sources, fail]);

  // ---- 系统调整（WinOps）----
  const doTweakScan = useCallback(async () => {
    setTweaksLoading(true);
    setErr(null);
    try {
      setTweaks(await winopsScan());
      setRegressed([]); // 手动重扫后黄条消除
    } catch (e) {
      fail(e);
    } finally {
      setTweaksLoading(false);
    }
  }, [fail]);

  const doTweakApply = useCallback(
    async (id: string) => {
      const name = tweaks.find(([w]) => w.id === id)?.[0].name ?? id;
      if (
        !(await confirmAction({
          title: `应用系统调整「${name}」`,
          impact: ["将修改 1 项系统设置"],
          detail: "原值自动备份（BAVR），校验失败自动回滚。",
          danger: false,
          confirmLabel: "应用",
        }))
      )
        return;
      setBusyTweak(id);
      setErr(null);
      try {
        await winopsApply(id);
        setMsg(`已应用（原值已备份，可回滚）`);
        await doTweakScan();
      } catch (e) {
        fail(e);
      } finally {
        setBusyTweak(null);
      }
    },
    [tweaks, doTweakScan, fail],
  );

  const doTweakRollback = useCallback(
    async (id: string) => {
      const name = tweaks.find(([w]) => w.id === id)?.[0].name ?? id;
      if (
        !(await confirmAction({
          title: `回滚系统调整「${name}」`,
          impact: ["将恢复该设置到最近一次应用前的状态"],
          detail: "当前值将被覆盖。",
          danger: false,
          confirmLabel: "回滚",
        }))
      )
        return;
      setBusyTweak(id);
      setErr(null);
      try {
        await winopsRollback(id);
        setMsg("已回滚到最近一次应用前的状态");
        await doTweakScan();
      } catch (e) {
        fail(e);
      } finally {
        setBusyTweak(null);
      }
    },
    [tweaks, doTweakScan, fail],
  );

  // W7 审计导出：审计记录 + 备份清单 → {appData}/winops/exports/
  const doAuditExport = useCallback(async () => {
    setErr(null);
    try {
      const path = await winopsAuditExport();
      setMsg(`审计已导出：${path}`);
    } catch (e) {
      fail(e);
    }
  }, [fail]);

  // 浏览目录（T-B1-9）：winops_catalog 不经扫描即可通读全目录，按 category 分组
  const openCatalog = useCallback(() => {
    setCatalogOpen(true);
    setCatalog(null);
    void winopsCatalog()
      .then(setCatalog)
      .catch(fail);
  }, [fail]);

  // ---- 进程页（T-B7-10）----
  /** 两拍载入：第一拍立差值基线（全 0 轮），越过 PROCESS_SAMPLE_GAP_MS 后第二拍取真差值行 */
  const loadProcs = useCallback(
    async (sort: string, query: string) => {
      setProcBusy(true);
      try {
        await sysProcesses(sort, query);
        await new Promise((r) => setTimeout(r, 600));
        setProcs(await sysProcesses(sort, query));
      } catch (e) {
        fail(e);
      } finally {
        setProcBusy(false);
      }
    },
    [fail],
  );

  useEffect(() => {
    void loadProcs("cpu", "");
  }, [loadProcs]);

  /** 复述名逐字相符（与后端 eq_ignore_ascii_case 同谱的大小写宽容）才放行确认钮 */
  const killTypedOk =
    killTarget !== null && killTyped.trim().toLowerCase() === killTarget.name.toLowerCase();

  const confirmKill = useCallback(async () => {
    if (!killTarget || !killTypedOk) return;
    const typed = killTyped.trim();
    if (
      !(await confirmAction({
        title: "结束进程",
        impact: [
          `将结束进程「${killTarget.name}」（pid ${killTarget.pid}）`,
          `当前占用：CPU ${killTarget.cpu_pct.toFixed(1)}% · 内存 ${Math.round(killTarget.mem_bytes / 1024 / 1024)} MB`,
        ],
        detail:
          "TerminateProcess 立即强杀，进程未保存数据将丢失；系统关键进程受保护名单拦截。操作（成功与被拒均）落 sys/process_audit.jsonl 审计。",
        danger: true,
        confirmLabel: "结束进程",
      }))
    )
      return;
    setKillTarget(null);
    setKillTyped("");
    try {
      const name = await sysKill(killTarget.pid, typed);
      setMsg(`已结束进程「${name}」（已记 kill 审计）`);
      await loadProcs(procSort, procQuery);
    } catch (e) {
      fail(e);
    }
  }, [killTarget, killTypedOk, procSort, procQuery, loadProcs, fail]);

  const cpuSeries = history.map((p) => p.cpu);
  const memSeries = history.map((p) => (p.mem_total ? (p.mem_used / p.mem_total) * 100 : 0));
  const netSeries = history.map((p) => p.net_bps / 1024);
  const latest = history[history.length - 1];
  const targetById = new Map(targets.map((t) => [t.id, t]));
  // 目录按 category 分组（核账：分组键是 category 非 family），保持后端目录顺序
  const catalogGroups: [string, WinopsTweakDto[]][] = [];
  for (const tw of catalog ?? []) {
    const g = catalogGroups.find(([c]) => c === tw.category);
    if (g) g[1].push(tw);
    else catalogGroups.push([tw.category, [tw]]);
  }

  const fmtBytes = (b: number) =>
    b >= 1024 ** 3 ? `${(b / 1024 ** 3).toFixed(1)} GB` : b >= 1024 ** 2 ? `${(b / 1024 ** 2).toFixed(0)} MB` : `${(b / 1024).toFixed(0)} KB`;

  const shownPkgs = pkgs.filter((p) => {
    const q = pkgFilter.trim().toLowerCase();
    return !q || p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q);
  });

  return (
    <div className={styles.root}>
      <Tabs
        ariaLabel="系统管理视图"
        value={tab}
        onChange={setTab}
        items={[
          { id: "monitor", label: "资源监控" },
          { id: "clean", label: "系统清理" },
          { id: "pkg", label: "包管理" },
          { id: "tweaks", label: "系统调整" },
        ]}
      />

      <InlineError text={msg} tone="success" />
      <InlineError text={err} />

      {tab === "monitor" && (
        <>
          <Section>
            {!latest && <Spinner size="tiny" />}
            {latest && (
              <Text className={styles.muted}>
                内存 {fmtBytes(latest.mem_used)} / {fmtBytes(latest.mem_total)} ·{" "}
                {latest.disks.map((d) => `${d.mount} ${fmtBytes(d.total - d.used)} 可用`).join(" · ")}
              </Text>
            )}
            <div className={styles.row}>
              <div style={{ flex: 1, minWidth: 240 }}>
                <Spark values={cpuSeries} color={tokens.colorBrandForeground1} label="CPU" fmt={(v) => `${v.toFixed(1)}%`} />
              </div>
              <div style={{ flex: 1, minWidth: 240 }}>
                <Spark values={memSeries} color={tokens.colorPaletteGreenForeground1} label="内存占用" fmt={(v) => `${v.toFixed(1)}%`} />
              </div>
              <div style={{ flex: 1, minWidth: 240 }}>
                <Spark values={netSeries} color={tokens.colorPaletteMarigoldForeground1} label="网络吞吐" fmt={(v) => `${v.toFixed(1)} KB/s`} />
              </div>
            </div>
            <Text className={styles.muted}>1s PDH 采样 · 保留最近 300 点 · 无数据时确认模块已启动</Text>
          </Section>

          {/* 进程页（T-B7-10 红线）：两拍差值 Top-N + 排序头/搜索 + 结束钮复述名输入确认词 */}
          <Section
            title={`进程（${procs.length}）`}
            actions={
              <>
                <Input
                  style={{ maxWidth: 180 }}
                  placeholder="搜索进程名"
                  value={procQuery}
                  onChange={(_, d) => setProcQuery(d.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void loadProcs(procSort, procQuery);
                  }}
                />
                <Button size="small" onClick={() => void loadProcs(procSort, procQuery)}>
                  刷新
                </Button>
                {procBusy && <Spinner size="tiny" />}
              </>
            }
          >
            <div className={styles.row}>
              <Text className={styles.muted}>排序：</Text>
              {(
                [
                  ["name", "名称"],
                  ["cpu", "CPU%"],
                  ["mem", "内存"],
                  ["disk", "磁盘"],
                ] as const
              ).map(([key, label]) => (
                <Button
                  key={key}
                  size="small"
                  appearance={procSort === key ? "primary" : "subtle"}
                  onClick={() => {
                    setProcSort(key);
                    void loadProcs(key, procQuery);
                  }}
                >
                  {label}
                </Button>
              ))}
            </div>
            <div className={styles.list}>
              {procs.map((r) => (
                <div key={r.pid} style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <div className={styles.item}>
                    <div style={{ display: "flex", flexDirection: "column", minWidth: 0, flex: 1 }}>
                      <Text size={200} weight="semibold">
                        {r.name}
                      </Text>
                      <Text size={100} className={styles.muted}>
                        pid {r.pid} · CPU {r.cpu_pct.toFixed(1)}% · 内存 {fmtBytes(r.mem_bytes)} · 磁盘{" "}
                        {r.disk_bps === null ? "—" : `${fmtBytes(r.disk_bps)}/s`}
                      </Text>
                    </div>
                    <Button
                      size="small"
                      appearance="subtle"
                      disabled={killTarget !== null}
                      onClick={() => {
                        setKillTarget(r);
                        setKillTyped("");
                      }}
                    >
                      结束
                    </Button>
                  </div>
                  {killTarget?.pid === r.pid && (
                    <div className={styles.row} style={{ padding: "0 8px 6px" }}>
                      <Input
                        className="kill-confirm-input"
                        style={{ flex: 1, minWidth: 200 }}
                        placeholder={`逐字输入「${r.name}」以确认`}
                        value={killTyped}
                        onChange={(_, d) => setKillTyped(d.value)}
                      />
                      <Button
                        appearance="primary"
                        size="small"
                        disabled={!killTypedOk}
                        onClick={() => void confirmKill()}
                      >
                        确认结束
                      </Button>
                      <Button
                        size="small"
                        appearance="subtle"
                        onClick={() => {
                          setKillTarget(null);
                          setKillTyped("");
                        }}
                      >
                        取消
                      </Button>
                    </div>
                  )}
                </div>
              ))}
              {!procBusy && procs.length === 0 && (
                <Text className={styles.muted}>
                  进程列表为空（首轮为差值基线拍，稍候点「刷新」取两拍结果；非 Windows 端口缺省亦空表）
                </Text>
              )}
            </div>
            <Text className={styles.muted}>
              CPU/磁盘为两拍差值（首拍无基线记 0/—）· 结束进程须逐字复述进程名并经确认对话框 · 系统关键进程（pid 0/4 与保护名单）拒绝结束 · 成功与被拒均落审计
            </Text>
          </Section>
        </>
      )}

      {tab === "clean" && (
        <Section>
          <div className={styles.row}>
            <Button appearance="primary" size="small" onClick={() => void doScan()}>
              {scanning ? "扫描中…" : "扫描"}
            </Button>
            {scanning && <Spinner size="tiny" />}
            <Checkbox
              label="移入回收站（可恢复）"
              checked={useRecycle}
              onChange={(_, d) => setUseRecycle(!!d.checked)}
            />
            <div className={styles.grow} />
            <Button
              size="small"
              appearance="primary"
              disabled={scan.length === 0 || selected.size === 0}
              title={scan.length === 0 ? "未扫描仅见清单，扫描后方可执行" : undefined}
              onClick={() => void doExecute()}
            >
              执行清理（{selected.size} 项）
            </Button>
          </div>
          {scan.length === 0 && (
            <Text className={styles.muted}>
              未扫描仅见清单：勾选是持久偏好（「推荐」= safe_default 角标，不自动勾选），扫描后方可执行；24h 内修改的文件自动跳过
            </Text>
          )}
          <div className={styles.list}>
            {scan.length === 0
              ? targets.map((t) => (
                  <div key={t.id} className={styles.item}>
                    <Checkbox
                      checked={selected.has(t.id)}
                      onChange={(_, d) => toggleTarget(t.id, !!d.checked)}
                    />
                    <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
                      <Text size={200} weight="semibold">
                        {t.label}
                        {t.safe_default && (
                          <Badge size="small" appearance="filled" color="brand" style={{ marginLeft: 6 }}>
                            推荐
                          </Badge>
                        )}
                        {t.need_admin && (
                          <Badge size="small" appearance="outline" style={{ marginLeft: 6 }}>
                            需管理员
                          </Badge>
                        )}
                        {t.optional && (
                          <Badge size="small" appearance="outline" color="subtle" style={{ marginLeft: 6 }}>
                            缺失自动跳过
                          </Badge>
                        )}
                      </Text>
                      <Text size={100} className={styles.muted}>
                        目录 {t.dir} · 范围 {t.exts.length ? `仅 .${t.exts.join("/.")}` : "全部文件"}
                      </Text>
                    </div>
                  </div>
                ))
              : scan.map((s) => {
                  const t = targetById.get(s.target_id);
                  return (
                    <div key={s.target_id} className={styles.item}>
                      <Checkbox
                        checked={selected.has(s.target_id)}
                        disabled={s.missing || s.files === 0}
                        onChange={(_, d) => toggleTarget(s.target_id, !!d.checked)}
                      />
                      <div style={{ display: "flex", flexDirection: "column", minWidth: 0 }}>
                        <Text size={200} weight="semibold">
                          {s.label}
                          {s.safe_default && (
                            <Badge size="small" appearance="filled" color="brand" style={{ marginLeft: 6 }}>
                              推荐
                            </Badge>
                          )}
                          {s.need_admin && (
                            <Badge size="small" appearance="outline" style={{ marginLeft: 6 }}>
                              管理员
                            </Badge>
                          )}
                        </Text>
                        <Text size={100} className={styles.muted}>
                          {s.missing ? (
                            "目录不存在"
                          ) : (
                            <>
                              可清理 {s.files} 文件 / {fmtBytes(s.reclaim_bytes)}
                              {s.skipped_recent > 0 && ` · 白名单跳过 ${s.skipped_recent}（24h 内修改）`}
                            </>
                          )}
                          {t && ` —— 目录 ${t.dir} · 范围 ${t.exts.length ? `仅 .${t.exts.join("/.")}` : "全部文件"}`}
                        </Text>
                      </div>
                    </div>
                  );
                })}
            {scan.length === 0 && targets.length === 0 && (
              <Text className={styles.muted}>清理目标清单加载中（内置 4 项）</Text>
            )}
          </div>
        </Section>
      )}

      {tab === "pkg" && (
        <Section>
          <div className={styles.row}>
            <Text size={200} weight="semibold">
              源：
            </Text>
            {sources.map((s) => (
              <Badge key={s.id} appearance={s.available ? "filled" : "outline"} color={s.available ? "success" : "subtle"}>
                {s.id}
              </Badge>
            ))}
            <div className={styles.grow} />
            <Input
              style={{ maxWidth: 220 }}
              placeholder="搜索已装"
              value={pkgFilter}
              onChange={(_, d) => setPkgFilter(d.value)}
            />
            <Button size="small" appearance="primary" onClick={() => void loadPkgs()}>
              刷新清单
            </Button>
            <Button
              size="small"
              onClick={() => {
                const src = sources.find((s) => s.available);
                if (src) void doPkgAction(src.id, "upgrade_all", "");
                else setErr("无可用包管理器");
              }}
            >
              全部升级
            </Button>
            {pkgsLoading && <Spinner size="tiny" />}
          </div>
          <Text className={styles.muted}>
            已装 {shownPkgs.length} 项（多源去重，winget 优先）· 升级可用标绿色
          </Text>
          <div className={styles.list}>
            {shownPkgs.slice(0, 200).map((p) => (
              <div key={`${p.source}:${p.id}`} className={styles.item}>
                <div style={{ display: "flex", flexDirection: "column", minWidth: 0, flex: 1 }}>
                  <Text size={200} weight="semibold">
                    {p.name}
                    {p.available && (
                      <Badge size="small" appearance="filled" color="success" style={{ marginLeft: 6 }}>
                        {p.version} → {p.available}
                      </Badge>
                    )}
                  </Text>
                  <Text size={100} className={styles.muted}>
                    {p.id} · {p.version} · {p.source}
                  </Text>
                </div>
                <Button
                  size="small"
                  appearance="subtle"
                  onClick={() => void doPkgAction(p.source, "install", p.id)}
                >
                  安装
                </Button>
                <Button
                  size="small"
                  appearance="subtle"
                  title={`单包升级（${p.source} upgrade ${p.id}）`}
                  onClick={() => void doPkgAction(p.source, "upgrade", p.id)}
                >
                  升级
                </Button>
                <Button
                  size="small"
                  appearance="subtle"
                  onClick={() => void doPkgAction(p.source, "uninstall", p.id)}
                >
                  卸载
                </Button>
              </div>
            ))}
            {!pkgsLoading && shownPkgs.length === 0 && (
              <Text className={styles.muted}>点击「刷新清单」获取已装软件（需要 winget/scoop/choco 至少一个可用）</Text>
            )}
          </div>
          {/* T-B7-12 在线搜索：结果表安装钮复用确切命令行确认对话框（在线安装对话框） */}
          <div className={styles.row}>
            <Text size={200} weight="semibold">
              在线搜索：
            </Text>
            <Input
              style={{ maxWidth: 220 }}
              placeholder="包名关键词（可含空格引号，换行拒）"
              value={onlineQuery}
              onChange={(_, d) => setOnlineQuery(d.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void doOnlineSearch();
              }}
            />
            <Button size="small" appearance="primary" disabled={onlineBusy} onClick={() => void doOnlineSearch()}>
              搜索
            </Button>
            {onlineBusy && <Spinner size="tiny" />}
          </div>
          {onlineResults !== null && (
            <div className={styles.list} data-testid="pkg-search-results">
              <Text className={styles.muted}>
                搜索结果 {onlineResults.length} 项 · 安装走确切命令行确认（预览与执行同源）
              </Text>
              {onlineResults.slice(0, 100).map((r) => (
                <div key={`${r.source}:${r.id}`} className={styles.item}>
                  <div style={{ display: "flex", flexDirection: "column", minWidth: 0, flex: 1 }}>
                    <Text size={200} weight="semibold">
                      {r.name}
                    </Text>
                    <Text size={100} className={styles.muted}>
                      {r.id} · {r.version} · {r.source}
                    </Text>
                  </div>
                  <Button size="small" onClick={() => void doPkgAction(r.source, "install", r.id)}>
                    安装
                  </Button>
                </div>
              ))}
              {onlineResults.length === 0 && (
                <Text className={styles.muted}>无搜索结果（核对关键词；winget 无匹配时亦空表）</Text>
              )}
            </div>
          )}
          {output.length > 0 && (
            <div className={styles.log}>{output.slice(-30).join("\n")}</div>
          )}
        </Section>
      )}

      {tab === "tweaks" && (
        <Section>
          {regressed.length > 0 && (
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 10px",
                marginBottom: 8,
                borderRadius: 4,
                background: tokens.colorPaletteYellowBackground1,
              }}
            >
              <Text size={200}>
                系统已将 {regressed.length} 项设置恢复为默认（自愈），可重新应用。
              </Text>
              <Button size="small" appearance="subtle" onClick={() => void doTweakScan()}>
                重新扫描
              </Button>
            </div>
          )}
          <div className={styles.row}>
            <Button appearance="primary" size="small" onClick={() => void doTweakScan()}>
              {tweaksLoading ? "扫描中…" : "扫描"}
            </Button>
            {tweaksLoading && <Spinner size="tiny" />}
            <Button size="small" appearance="subtle" onClick={() => void doAuditExport()}>
              导出审计
            </Button>
            <Button
              size="small"
              appearance="outline"
              title="不经扫描通读全目录：按分类列出说明、提权要求与维护型标记"
              onClick={openCatalog}
            >
              浏览目录
            </Button>
            <Text className={styles.muted}>
              BAVR 语义：应用前自动备份原值 · 校验失败自动回滚 · 回滚恢复最近一次应用前的状态
            </Text>
          </div>
          <div className={styles.list}>
            {tweaks.map(([t, state]) => (
              <div key={t.id} className={styles.item}>
                <div style={{ display: "flex", flexDirection: "column", minWidth: 0, flex: 1 }}>
                  <Text size={200} weight="semibold">
                    {t.name}
                    <Badge size="small" appearance="outline" style={{ marginLeft: 6 }}>
                      {t.category}
                    </Badge>
                    {state === "applied" && (
                      <Badge size="small" appearance="filled" color="success" style={{ marginLeft: 6 }}>
                        已应用
                      </Badge>
                    )}
                    {state === "not_applied" && (
                      <Badge size="small" appearance="outline" style={{ marginLeft: 6 }}>
                        未应用
                      </Badge>
                    )}
                    {state === "needs_admin" && (
                      <Badge size="small" appearance="filled" color="warning" style={{ marginLeft: 6 }}>
                        需管理员
                      </Badge>
                    )}
                  </Text>
                  {t.description && (
                    <Text size={100} className={styles.muted}>
                      {t.description}
                    </Text>
                  )}
                </div>
                {state === "not_applied" && (
                  <Button
                    size="small"
                    appearance="primary"
                    disabled={busyTweak !== null}
                    onClick={() => void doTweakApply(t.id)}
                  >
                    {busyTweak === t.id ? "应用中…" : "应用"}
                  </Button>
                )}
                {state !== "not_applied" && (
                  <Button
                    size="small"
                    appearance="subtle"
                    disabled={busyTweak !== null}
                    onClick={() => void doTweakRollback(t.id)}
                  >
                    {busyTweak === t.id ? "处理中…" : "回滚"}
                  </Button>
                )}
              </div>
            ))}
            {!tweaksLoading && tweaks.length === 0 && (
              <Text className={styles.muted}>点击「扫描」检查各调整项的当前状态（目录支持外置扩展：{`{appData}/winops/catalog/*.json`}）</Text>
            )}
          </div>
        </Section>
      )}

      {/* 浏览目录 Dialog（T-B1-9）：category 分组 + 提权/维护徽标 + 生效方式映射 */}
      <Dialog
        open={catalogOpen}
        onOpenChange={(_, d) => {
          if (!d.open) setCatalogOpen(false);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>系统调整目录（按分类）</DialogTitle>
            <DialogContent>
              {catalog === null ? (
                <Spinner size="tiny" />
              ) : (
                <div className={styles.list}>
                  {catalogGroups.map(([cat, list]) => (
                    <div key={cat}>
                      <Text size={200} weight="semibold">
                        {cat}（{list.length}）
                      </Text>
                      {list.map((tw) => (
                        <div key={tw.id} style={{ padding: "4px 8px 8px 12px" }}>
                          <Text size={200}>
                            {tw.name}
                            {tw.requires_admin && (
                              <Badge size="small" appearance="filled" color="warning" style={{ marginLeft: 6 }}>
                                需管理员
                              </Badge>
                            )}
                            {tw.maintenance && (
                              <Badge size="small" appearance="outline" color="subtle" style={{ marginLeft: 6 }}>
                                维护型
                              </Badge>
                            )}
                          </Text>
                          {tw.description && (
                            <Text size={100} className={styles.muted}>
                              {tw.description}
                            </Text>
                          )}
                          <Text size={100} className={styles.muted}>
                            {winopsEffectHint(tw.actions)}
                          </Text>
                        </div>
                      ))}
                    </div>
                  ))}
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setCatalogOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
