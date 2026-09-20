import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Dropdown,
  Input,
  Option,
  Textarea,
  Spinner,
  Table,
  TableBody,
  TableCell,
  TableRow,
} from "@fluentui/react-components";
import {
  proxyDelayTest,
  proxyDirectRules,
  proxyKernelInstall,
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
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 代理面板（docs/impl/05 PR6，M7 v1）：
 * ① 模式三态（关闭/系统代理/TUN，TUN 需管理员+wintun）② 内核/wintun 安装
 * ③ 订阅管理（增删改拉取）④ 节点列表 + TCP 延迟 ⑤ 直连规则 ⑥ 内核日志。
 * proxy.* 事件驱动刷新（state_changed/nodes_changed/log_line）。
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
  modeBtn: { minWidth: "96px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  logBox: {
    maxHeight: "180px",
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px",
  },
  grow: { flex: 1, minWidth: "240px" },
});

const MODE_LABEL: Record<ProxyStatusDto["mode"], string> = {
  off: "关闭",
  system: "系统代理",
  tun: "TUN 模式",
};

const fmtTime = (ms: number) =>
  ms > 0 ? new Date(ms).toLocaleTimeString() : "未拉取";

export default function ProxyPanel() {
  const styles = useStyles();
  const [status, setStatus] = useState<ProxyStatusDto | null>(null);
  const [subs, setSubs] = useState<ProxySubDto[]>([]);
  const [nodes, setNodes] = useState<ProxyNodeDto[]>([]);
  const [delays, setDelays] = useState<Record<string, number | null>>({});
  const [rulesText, setRulesText] = useState("");
  const [logs, setLogs] = useState<ProxyLogLineDto[]>([]);
  const [subName, setSubName] = useState("");
  const [subUrl, setSubUrl] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  // 首轮数据/日志是否落定：未落定前空列表渲染加载态而非"暂无"文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  const [logsLoaded, setLogsLoaded] = useState(false);
  const mounted = useRef(true);

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
            const line = (e.payload as { payload?: ProxyLogLineDto }).payload;
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

  const run = useCallback(
    async (key: string, action: () => Promise<unknown>) => {
      setBusy(key);
      try {
        await action();
        setError("");
      } catch (e) {
        setError(parseAppError(e)?.data.message ?? String(e));
      } finally {
        if (mounted.current) setBusy("");
      }
    },
    [],
  );

  const switchMode = (mode: ProxyStatusDto["mode"]) =>
    run("mode", async () => {
      await proxySetMode(mode);
      await refresh();
    });

  // 换核生命周期全在后端（运行中=起新核、失败自动回滚旧核并上抛原错），UI 只刷新如实状态
  const selectKernel = (id: string) =>
    run("kernel-select", async () => {
      await proxyKernelSelect(id);
      await refresh();
    });

  const delayKey = (d: ProxyNodeDelayDto) => `${d.sub_id}|${d.tag}`;

  const testDelays = () =>
    run("delay", async () => {
      const result = await proxyDelayTest();
      const map: Record<string, number | null> = {};
      for (const d of result) map[delayKey(d)] = d.ms;
      if (mounted.current) setDelays(map);
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

  const st = status;

  return (
    <div className={styles.root}>
      {st?.restored_last_run && (
        <Section>
          <Text size={200}>
            检测到上次异常退出残留的系统代理，启动时已自动还原为你的原始设置。
          </Text>
        </Section>
      )}

      {/* 模式三态 */}
      <Section
        title="运行模式"
        actions={
          <>
            {st && (
              <Badge appearance={st.kernel_running ? "filled" : "outline"} color={st.kernel_running ? "success" : "subtle"}>
                {st.kernel_running ? "内核运行中" : "内核已停止"}
              </Badge>
            )}
            {st?.has_backup && <Badge appearance="outline">存在原设置备份</Badge>}
            {/* T-B2-2 最小选择器（完整内核卡归 T-B2-3）：只按后端注册表渲染，禁前端内核特例分支 */}
            {st && (
              <Dropdown
                size="small"
                style={{ minWidth: "148px" }}
                disabled={busy !== ""}
                value={`内核：${st.kernels.find((k) => k.id === st.kernel)?.display_name ?? st.kernel}`}
                selectedOptions={[st.kernel]}
                onOptionSelect={(_, d) => {
                  const id = String(d.optionValue ?? "");
                  if (id !== "" && id !== st.kernel) void selectKernel(id);
                }}
              >
                {st.kernels.map((k) => (
                  <Option key={k.id} value={k.id} text={k.display_name}>
                    {k.display_name}
                    {k.id === st.kernel ? "（当前）" : k.installed ? "" : "（未安装）"}
                  </Option>
                ))}
              </Dropdown>
            )}
          </>
        }
      >
        <div className={styles.row}>
          {(["off", "system", "tun"] as const).map((m) => (
            <Button
              key={m}
              className={styles.modeBtn}
              appearance={st?.mode === m ? "primary" : "outline"}
              disabled={busy !== "" || (m === "tun" && st != null && !st.admin)}
              onClick={() => switchMode(m)}
            >
              {busy === "mode" && st?.mode !== m ? <Spinner size="tiny" /> : MODE_LABEL[m]}
            </Button>
          ))}
          {st && !st.admin && (
            <span className={styles.muted}>TUN 需以管理员身份运行</span>
          )}
        </div>
        {st && (
          <span className={styles.muted}>
            入站 127.0.0.1:{st.inbound_port} · 节点 {st.nodes_total} · 订阅 {st.subs_total}
            {st.mode === "tun" ? " · TUN 与系统代理互斥（已自动还原系统代理）" : ""}
          </span>
        )}
      </Section>

      {/* 内核安装 */}
      <Section
        title="sing-box 内核"
        actions={
          <>
            {st?.kernel_installed ? (
              <Badge appearance="outline">v{st.kernel_version ?? "?"}</Badge>
            ) : (
              <Badge appearance="outline" color="warning">
                未安装
              </Badge>
            )}
            {st?.wintun_installed && <Badge appearance="outline">wintun 已装</Badge>}
          </>
        }
      >
        <div className={styles.row}>
          <Button
            size="small"
            disabled={busy !== ""}
            onClick={() => run("kernel", async () => { await proxyKernelInstall(); await refresh(); })}
          >
            {st?.kernel_installed ? "重新下载内核" : "下载内核"}
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || st?.wintun_installed === true}
            onClick={() => run("wintun", async () => { await proxyWintunInstall(); await refresh(); })}
          >
            安装 wintun.dll（TUN 前置）
          </Button>
          {busy === "kernel" && <span className={styles.muted}>正在从官方 Release 下载…</span>}
        </div>
        <span className={styles.muted}>
          内核按需下载，不随软件分发；仅支持本地编排，不内置任何节点/订阅。
        </span>
      </Section>

      {/* 订阅管理 */}
      <Section
        title="订阅"
        actions={
          <span className={styles.muted}>自行添加分享链接或订阅地址（ss/vmess/trojan/vless）</span>
        }
      >
        <div className={styles.row}>
          <Input
            className={styles.grow}
            placeholder="名称（可选）"
            value={subName}
            onChange={(_, d) => setSubName(d.value)}
            size="small"
          />
          <Input
            className={styles.grow}
            placeholder="订阅 URL / 分享链接"
            value={subUrl}
            onChange={(_, d) => setSubUrl(d.value)}
            size="small"
          />
          <Button
            size="small"
            appearance="primary"
            disabled={busy !== "" || subUrl.trim() === ""}
            onClick={() =>
              run("sub-add", async () => {
                const sub = await proxySubAdd(subName, subUrl);
                setSubName("");
                setSubUrl("");
                await proxySubUpdate(sub.id).catch((e) =>
                  reportError(e, { context: "订阅添加后节点更新失败" }),
                );
                await refresh();
              })
            }
          >
            添加并拉取
          </Button>
        </div>
        {subs.map((s) => (
          <div key={s.id} className={styles.row}>
            <Text size={300} weight="semibold" style={{ minWidth: "120px" }}>
              {s.name}
            </Text>
            <span className={styles.mono}>{s.node_count} 节点 · {fmtTime(s.updated_ms)}</span>
            <span className={styles.grow} />
            <Button
              size="small"
              disabled={busy !== ""}
              onClick={() =>
                run(`sub-upd-${s.id}`, async () => {
                  await proxySubUpdate(s.id);
                  await refresh();
                })
              }
            >
              {busy === `sub-upd-${s.id}` ? "拉取中…" : "更新"}
            </Button>
            <Button
              size="small"
              disabled={busy !== ""}
              onClick={() => void removeSub(s)}
            >
              删除
            </Button>
          </div>
        ))}
      </Section>

      {/* 节点列表 */}
      <Section
        title="节点"
        actions={
          <Button size="small" disabled={busy !== "" || nodes.length === 0} onClick={testDelays}>
            {busy === "delay" ? "测速中…" : "测速（TCP）"}
          </Button>
        }
      >
        {nodes.length === 0 ? (
          <EmptyState text="暂无节点：请先添加订阅并拉取" loading={!loaded} />
        ) : (
          <Table size="small">
            <TableBody>
              {nodes.map((n) => {
                const ms = delays[`${n.sub_id}|${n.tag}`];
                return (
                  <TableRow key={`${n.sub_id}|${n.tag}`}>
                    <TableCell>
                      <span className={styles.mono}>{n.tag}</span>
                    </TableCell>
                    <TableCell>{n.kind}</TableCell>
                    <TableCell>
                      <span className={styles.mono}>
                        {n.server}:{n.port}
                      </span>
                    </TableCell>
                    <TableCell>
                      {ms === undefined ? (
                        <span className={styles.muted}>—</span>
                      ) : ms === null ? (
                        <Badge appearance="outline" color="danger">
                          不可达
                        </Badge>
                      ) : (
                        <Badge appearance="outline" color="success">
                          {ms} ms
                        </Badge>
                      )}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </Section>

      {/* 直连规则 */}
      <Section
        title="直连域名"
        actions={
          <span className={styles.muted}>每行一个后缀，命中即不走代理（重新切换模式后生效）</span>
        }
      >
        <Textarea
          value={rulesText}
          onChange={(_, d) => setRulesText(d.value)}
          rows={4}
          placeholder={"cn\baidu.com\nbilibili.com"}
        />
        <div className={styles.row}>
          <Button
            size="small"
            disabled={busy !== ""}
            onClick={() =>
              run("rules", async () => {
                await proxySetDirectRules(rulesText.split("\n"));
                await refresh();
              })
            }
          >
            保存规则
          </Button>
        </div>
      </Section>

      {/* 内核日志 */}
      <Section
        title="内核日志"
        actions={
          <Button size="small" onClick={() => void refreshLogs()}>
            刷新
          </Button>
        }
      >
        <div className={styles.logBox}>
          {logs.length === 0 ? (
            <EmptyState text="暂无日志（内核未启动或未产生输出）" loading={!logsLoaded} />
          ) : (
            logs.map((l, i) => (
              <div key={`${l.ts_ms}-${i}`} className={styles.mono}>
                {l.text}
              </div>
            ))
          )}
        </div>
      </Section>

      <InlineError text={error} />
    </div>
  );
}
