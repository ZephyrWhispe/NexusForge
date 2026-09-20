import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles } from "@fluentui/react-components";
import {
  proxyDelayTest,
  proxyDirectRules,
  proxyKernelInstall,
  proxyKernelRestart,
  proxyKernelSelect,
  proxyLogs,
  proxyNodes,
  proxySetDirectRules,
  proxySetMode,
  proxyStatus,
  proxySubAdd,
  proxySubRemove,
  proxySubs,
  proxySubUpdate,
  proxyWintunInstall,
  parseAppError,
  type ProxyLogLineDto,
  type ProxyNodeDelayDto,
  type ProxyNodeDto,
  type ProxyStatusDto,
  type ProxySubDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { isProxySubPanel, useSession } from "../../stores/session";
import InlineError from "../../components/InlineError";
import OverviewSection from "./panels/OverviewSection";
import NodesSection from "./panels/NodesSection";
import SubsSection from "./panels/SubsSection";
import RulesSection from "./panels/RulesSection";
import KernelSection from "./panels/KernelSection";
import LogsSection from "./panels/LogsSection";

/**
 * 代理面板（D-29 B2 T-B2-3，细案 02§2）：子面板化后的纯数据枢纽——
 * 状态/事件监听/防重 busy 全部留在根，六个子面板（总览/节点/订阅/分流/内核/日志）
 * 按 session store 的 proxySubPanel 键一次只渲染一个；选择入口在 SubNav（modules.ts）。
 * proxy.* 事件驱动刷新（state_changed/nodes_changed/log_line）不变。
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
});

export default function ProxyPanel() {
  const styles = useStyles();
  const [status, setStatus] = useState<ProxyStatusDto | null>(null);
  const [subs, setSubs] = useState<ProxySubDto[]>([]);
  const [nodes, setNodes] = useState<ProxyNodeDto[]>([]);
  const [delays, setDelays] = useState<Record<string, number | null>>({});
  const [rulesText, setRulesText] = useState("");
  const [logs, setLogs] = useState<ProxyLogLineDto[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  // 首轮数据/日志是否落定：未落定前空列表渲染加载态而非"暂无"文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  const [logsLoaded, setLogsLoaded] = useState(false);
  const mounted = useRef(true);

  // 子面板选择态（T-B2-3）：session store 键 proxySubPanel，旧快照缺键由 zustand
  // 浅合并回退初始值 overview；渲染侧再经 isProxySubPanel 收窄防野值。
  const storedSub = useSession((s) => s.proxySubPanel);
  const view = isProxySubPanel(storedSub) ? storedSub : "overview";

  const refresh = useCallback(async () => {
    try {
      const [st, ss, ns, rs] = await Promise.all([
        proxyStatus(),
        proxySubs(),
        proxyNodes(),
        proxyDirectRules(),
      ]);
      if (!mounted.current) return;
      setStatus(st);
      setSubs(ss);
      setNodes(ns);
      setRulesText(rs.join("\n"));
      setError("");
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setLoaded(true);
    }
  }, []);

  const refreshLogs = useCallback(async () => {
    try {
      const lines = await proxyLogs(200);
      if (mounted.current) setLogs(lines);
    } catch (e) {
      reportError(e, { context: "内核日志拉取失败", dedupeKey: "proxy-logs", toast: false });
    } finally {
      if (mounted.current) setLogsLoaded(true);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    void refreshLogs();
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic ?? "";
          if (topic === "proxy.state_changed" || topic === "proxy.nodes_changed") {
            void refresh();
          } else if (topic === "proxy.log_line") {
            const line = (e.payload as { payload?: { ts_ms: number; text: string } }).payload;
            if (line) setLogs((prev) => [...prev.slice(-199), line]);
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
      .catch((e) => reportError(e, { context: "代理面板事件监听注册失败", toast: false }));
    return () => {
      cancelled = true;
      mounted.current = false;
      unlisten?.();
    };
  }, [refresh, refreshLogs]);

  // busy 防重 + 错误归一；返回是否成功（SubsSection 依此决定是否清空输入框）
  const run = useCallback(
    async (key: string, action: () => Promise<unknown>): Promise<boolean> => {
      setBusy(key);
      try {
        await action();
        setError("");
        return true;
      } catch (e) {
        setError(parseAppError(e)?.data.message ?? String(e));
        return false;
      } finally {
        if (mounted.current) setBusy("");
      }
    },
    [],
  );

  const switchMode = (mode: ProxyStatusDto["mode"]) =>
    void run("mode", async () => {
      await proxySetMode(mode);
      await refresh();
    });

  const delayKey = (d: ProxyNodeDelayDto) => `${d.sub_id}|${d.tag}`;

  const testDelays = () =>
    void run("delay", async () => {
      const result = await proxyDelayTest();
      const map: Record<string, number | null> = {};
      for (const d of result) map[delayKey(d)] = d.ms;
      if (mounted.current) setDelays(map);
    });

  const addSub = async (name: string, url: string) =>
    run("sub-add", async () => {
      const sub = await proxySubAdd(name, url);
      await proxySubUpdate(sub.id).catch((e) =>
        reportError(e, { context: "订阅添加后节点更新失败" }),
      );
      await refresh();
    });

  const updateSub = (id: string) =>
    void run(`sub-upd-${id}`, async () => {
      await proxySubUpdate(id);
      await refresh();
    });

  // 删除订阅（D-18）：连带其下全部节点配置一并消失，影响面按 node_count 明示
  const removeSub = async (sub: ProxySubDto) => {
    if (
      !(await confirmAction({
        title: "删除订阅",
        impact: [
          `将删除订阅「${sub.name}」及其 ${sub.node_count} 个节点`,
          sub.node_count > 0
            ? "这些节点将从节点列表与测速结果中移除"
            : "该订阅尚未拉取到节点",
        ],
        detail: "订阅 URL 与已解析的节点配置一并删除，需重新添加并拉取才能恢复。",
        confirmLabel: "删除",
      }))
    )
      return;
    await run(`sub-del-${sub.id}`, async () => {
      await proxySubRemove(sub.id);
      await refresh();
    });
  };

  const saveRules = () =>
    void run("rules", async () => {
      await proxySetDirectRules(rulesText.split("\n"));
      await refresh();
    });

  // 换核生命周期全在后端（运行中=起新核、失败自动回滚旧核并上抛原错），UI 只刷新如实状态
  const selectKernel = (id: string) =>
    run("kernel-select", async () => {
      await proxyKernelSelect(id);
      await refresh();
    });

  // 安装钮点名内核（T-B2-3 扩参）：内核卡行内 version 留空原样透传 undefined
  // （后端 Option<String> 缺省=默认版本；前端不把"未填"伪造为 null——任务书字面判据）
  const installKernel = (kernel: string, version?: string) =>
    run("kernel-install", async () => {
      await proxyKernelInstall(kernel, version);
      await refresh();
    });

  // 重启=按当前模式保配置起新核（仅 kernel_running 时入口可点）；失败后端归零为关闭态
  const restartKernel = () =>
    run("kernel-restart", async () => {
      await proxyKernelRestart();
      await refresh();
    });

  const wintunInstall = () =>
    run("wintun", async () => {
      await proxyWintunInstall();
      await refresh();
    });

  return (
    <div className={styles.root}>
      {view === "overview" && (
        <OverviewSection st={status} busy={busy} onMode={switchMode} />
      )}
      {view === "nodes" && (
        <NodesSection
          nodes={nodes}
          delays={delays}
          busy={busy}
          loaded={loaded}
          onTestDelays={testDelays}
        />
      )}
      {view === "subs" && (
        <SubsSection
          subs={subs}
          busy={busy}
          onAdd={addSub}
          onUpdate={updateSub}
          onRemove={(s) => void removeSub(s)}
        />
      )}
      {view === "rules" && (
        <RulesSection
          rulesText={rulesText}
          setRulesText={setRulesText}
          busy={busy}
          onSave={saveRules}
        />
      )}
      {view === "kernel" && (
        <KernelSection
          st={status}
          nodes={nodes}
          busy={busy}
          onInstall={installKernel}
          onSelect={selectKernel}
          onRestart={restartKernel}
          onWintun={wintunInstall}
        />
      )}
      {view === "logs" && (
        <LogsSection
          logs={logs}
          logsLoaded={logsLoaded}
          onRefresh={() => void refreshLogs()}
          onClear={() => setLogs([])}
        />
      )}
      <InlineError text={error} />
    </div>
  );
}
