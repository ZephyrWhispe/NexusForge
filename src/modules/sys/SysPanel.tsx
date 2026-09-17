import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Checkbox,
  Spinner,
} from "@fluentui/react-components";
import {
  parseAppError,
  sysCleanExecute,
  sysCleanScan,
  sysMetricsHistory,
  sysPkgAction,
  sysPkgCmdPreview,
  sysPkgList,
  sysPkgSources,
  type CleanScanItemDto,
  type MetricsPointDto,
  type PkgEntryDto,
  type PkgSourceDto,
} from "../../ipc/client";

/**
 * 系统管理面板（docs/impl/06 SY1–SY4，M12 v1）：
 * - SY4 监控：CPU/内存/网络 1s 采样折线（SVG 自绘，300 点缓冲；uPlot 偏离为减依赖，可后换）
 * - SY3 清理：扫描（24h 白名单）→ 勾选 → 执行（回收站可恢复）
 * - SY1/SY2 包管理：winget/scoop/choco 源探测 + 合并清单 + 变更（确切命令行确认）
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
  grow: { flex: 1, minWidth: "120px" },
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
  confirm: {
    border: `1px solid ${tokens.colorPaletteYellowForeground1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
    backgroundColor: tokens.colorNeutralBackground2,
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
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

type TabId = "monitor" | "clean" | "pkg";

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

  // ---- 清理 ----
  const [scan, setScan] = useState<CleanScanItemDto[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [scanning, setScanning] = useState(false);
  const [useRecycle, setUseRecycle] = useState(true);

  // ---- 包管理 ----
  const [sources, setSources] = useState<PkgSourceDto[]>([]);
  const [pkgs, setPkgs] = useState<PkgEntryDto[]>([]);
  const [pkgFilter, setPkgFilter] = useState("");
  const [pkgsLoading, setPkgsLoading] = useState(false);
  const [pending, setPending] = useState<{ source: string; action: string; id: string; cmd: string } | null>(null);
  const [output, setOutput] = useState<string[]>([]);

  // 事件驱动：sys.metrics 1s 推送
  useEffect(() => {
    void sysMetricsHistory()
      .then((h) => {
        historyRef.current = h;
        setHistory(h);
      })
      .catch(() => {});
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
      setSelected(new Set(items.filter((i) => i.safe_default && i.files > 0).map((i) => i.target_id)));
    } catch (e) {
      fail(e);
    } finally {
      setScanning(false);
    }
  }, [fail]);

  const doExecute = useCallback(async () => {
    setErr(null);
    try {
      const n = await sysCleanExecute([...selected], useRecycle);
      setMsg(`已清理 ${n} 个文件${useRecycle ? "（移入回收站，可恢复）" : ""}`);
      await doScan();
    } catch (e) {
      fail(e);
    }
  }, [selected, useRecycle, doScan, fail]);

  const confirmAction = useCallback(
    async (source: string, action: string, id: string) => {
      setErr(null);
      try {
        const cmd = await sysPkgCmdPreview(source, action, id);
        setOutput([]);
        setPending({ source, action, id, cmd });
      } catch (e) {
        fail(e);
      }
    },
    [fail],
  );

  const runAction = useCallback(async () => {
    if (!pending) return;
    setErr(null);
    try {
      const lines = await sysPkgAction(pending.source, pending.action, pending.id);
      setMsg(`${pending.action} 完成（${lines.length} 行输出）`);
      setPending(null);
      if (pending.action !== "upgrade_all") await loadPkgs();
    } catch (e) {
      fail(e);
    }
  }, [pending, loadPkgs, fail]);

  const cpuSeries = history.map((p) => p.cpu);
  const memSeries = history.map((p) => (p.mem_total ? (p.mem_used / p.mem_total) * 100 : 0));
  const netSeries = history.map((p) => p.net_bps / 1024);
  const latest = history[history.length - 1];

  const fmtBytes = (b: number) =>
    b >= 1024 ** 3 ? `${(b / 1024 ** 3).toFixed(1)} GB` : b >= 1024 ** 2 ? `${(b / 1024 ** 2).toFixed(0)} MB` : `${(b / 1024).toFixed(0)} KB`;

  const shownPkgs = pkgs.filter((p) => {
    const q = pkgFilter.trim().toLowerCase();
    return !q || p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q);
  });

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <button className={`${styles.tab} ${tab === "monitor" ? styles.tabActive : ""}`} onClick={() => setTab("monitor")}>
          资源监控
        </button>
        <button className={`${styles.tab} ${tab === "clean" ? styles.tabActive : ""}`} onClick={() => setTab("clean")}>
          系统清理
        </button>
        <button className={`${styles.tab} ${tab === "pkg" ? styles.tabActive : ""}`} onClick={() => setTab("pkg")}>
          包管理
        </button>
      </div>

      {msg && <Text className={styles.ok}>{msg}</Text>}
      {err && <Text className={styles.error}>{err}</Text>}

      {tab === "monitor" && (
        <div className={styles.section}>
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
        </div>
      )}

      {tab === "clean" && (
        <div className={styles.section}>
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
              disabled={selected.size === 0}
              onClick={() => void doExecute()}
            >
              执行清理（{selected.size} 项）
            </Button>
          </div>
          <div className={styles.list}>
            {scan.map((s) => (
              <div key={s.target_id} className={styles.item}>
                <Checkbox
                  checked={selected.has(s.target_id)}
                  disabled={s.missing || s.files === 0}
                  onChange={(_, d) =>
                    setSelected((prev) => {
                      const next = new Set(prev);
                      if (d.checked) next.add(s.target_id);
                      else next.delete(s.target_id);
                      return next;
                    })
                  }
                />
                <Text size={200} weight="semibold">
                  {s.label}
                </Text>
                {s.need_admin && <Badge size="small" appearance="outline">管理员</Badge>}
                {s.missing ? (
                  <Text className={styles.muted}>目录不存在</Text>
                ) : (
                  <Text className={styles.muted}>
                    可清理 {s.files} 文件 / {fmtBytes(s.reclaim_bytes)}
                    {s.skipped_recent > 0 && ` · 白名单跳过 ${s.skipped_recent}（24h 内修改）`}
                  </Text>
                )}
              </div>
            ))}
            {scan.length === 0 && <Text className={styles.muted}>点击「扫描」统计各目录可回收空间（24h 内修改的文件自动跳过）</Text>}
          </div>
        </div>
      )}

      {tab === "pkg" && (
        <div className={styles.section}>
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
                if (src) void confirmAction(src.id, "upgrade_all", "");
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
                  onClick={() => void confirmAction(p.source, "install", p.id)}
                >
                  安装
                </Button>
                <Button
                  size="small"
                  appearance="subtle"
                  onClick={() => void confirmAction(p.source, "uninstall", p.id)}
                >
                  卸载
                </Button>
              </div>
            ))}
            {!pkgsLoading && shownPkgs.length === 0 && (
              <Text className={styles.muted}>点击「刷新清单」获取已装软件（需要 winget/scoop/choco 至少一个可用）</Text>
            )}
          </div>
          {pending && (
            <>
              <div className={styles.confirm}>{pending.cmd}</div>
              <div className={styles.row}>
                <Text className={styles.muted}>确认执行以上确切命令？</Text>
                <Button size="small" appearance="primary" onClick={() => void runAction()}>
                  确认执行
                </Button>
                <Button size="small" appearance="subtle" onClick={() => setPending(null)}>
                  取消
                </Button>
              </div>
            </>
          )}
          {output.length > 0 && (
            <div className={styles.log}>{output.slice(-30).join("\n")}</div>
          )}
        </div>
      )}
    </div>
  );
}
