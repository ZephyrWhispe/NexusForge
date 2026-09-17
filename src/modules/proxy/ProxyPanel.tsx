import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
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
  modeBtn: { minWidth: "96px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
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
    }
  }, []);

  const refreshLogs = useCallback(async () => {
    try {
      const lines = await proxyLogs(200);
      if (mounted.current) setLogs(lines);
    } catch {
      /* 日志失败不打扰主流程 */
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
      .catch(() => undefined);
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

  const delayKey = (d: ProxyNodeDelayDto) => `${d.sub_id}|${d.tag}`;

  const testDelays = () =>
    run("delay", async () => {
      const result = await proxyDelayTest();
      const map: Record<string, number | null> = {};
      for (const d of result) map[delayKey(d)] = d.ms;
      if (mounted.current) setDelays(map);
    });

  const st = status;

  return (
    <div className={styles.root}>
      {st?.restored_last_run && (
        <div className={styles.section}>
          <Text size={200}>
            检测到上次异常退出残留的系统代理，启动时已自动还原为你的原始设置。
          </Text>
        </div>
      )}

      {/* 模式三态 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">运行模式</Text>
          {st && (
            <Badge appearance={st.kernel_running ? "filled" : "outline"} color={st.kernel_running ? "success" : "subtle"}>
              {st.kernel_running ? "内核运行中" : "内核已停止"}
            </Badge>
          )}
          {st?.has_backup && <Badge appearance="outline">存在原设置备份</Badge>}
        </div>
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
      </div>

      {/* 内核安装 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">sing-box 内核</Text>
          {st?.kernel_installed ? (
            <Badge appearance="outline">v{st.kernel_version ?? "?"}</Badge>
          ) : (
            <Badge appearance="outline" color="warning">
              未安装
            </Badge>
          )}
          {st?.wintun_installed && <Badge appearance="outline">wintun 已装</Badge>}
        </div>
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
      </div>

      {/* 订阅管理 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">订阅</Text>
          <span className={styles.muted}>自行添加分享链接或订阅地址（ss/vmess/trojan/vless）</span>
        </div>
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
                await proxySubUpdate(sub.id).catch(() => undefined);
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
              onClick={() =>
                run(`sub-del-${s.id}`, async () => {
                  await proxySubRemove(s.id);
                  await refresh();
                })
              }
            >
              删除
            </Button>
          </div>
        ))}
      </div>

      {/* 节点列表 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">节点</Text>
          <span className={styles.grow} />
          <Button size="small" disabled={busy !== "" || nodes.length === 0} onClick={testDelays}>
            {busy === "delay" ? "测速中…" : "测速（TCP）"}
          </Button>
        </div>
        {nodes.length === 0 ? (
          <span className={styles.muted}>暂无节点：请先添加订阅并拉取</span>
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
      </div>

      {/* 直连规则 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">直连域名</Text>
          <span className={styles.muted}>每行一个后缀，命中即不走代理（重新切换模式后生效）</span>
        </div>
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
      </div>

      {/* 内核日志 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">内核日志</Text>
          <span className={styles.grow} />
          <Button size="small" onClick={() => void refreshLogs()}>
            刷新
          </Button>
        </div>
        <div className={styles.logBox}>
          {logs.length === 0 ? (
            <span className={styles.muted}>暂无日志（内核未启动或未产生输出）</span>
          ) : (
            logs.map((l, i) => (
              <div key={`${l.ts_ms}-${i}`} className={styles.mono}>
                {l.text}
              </div>
            ))
          )}
        </div>
      </div>

      {error && <span className={styles.error}>{error}</span>}
    </div>
  );
}
