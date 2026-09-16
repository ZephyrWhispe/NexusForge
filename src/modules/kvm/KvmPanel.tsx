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
  TableHeader,
  TableHeaderCell,
} from "@fluentui/react-components";
import {
  kvmConnectTo,
  kvmControlState,
  kvmDiscoveredPeers,
  kvmEdgeMap,
  kvmIssuePairCode,
  kvmPairedPeers,
  kvmPairWith,
  kvmReleaseControl,
  kvmSessionList,
  kvmSetEdgeMap,
  kvmUnpair,
  parseAppError,
  type ControlStateDto,
  type PairedPeerDto,
  type PeerInfoDto,
  type SessionDto,
} from "../../ipc/client";

/**
 * 键鼠共享面板（docs/impl/05 K8，M4 v1）：
 * ① 本端一次性码展示（对端输入用）② 发现设备配对 ③ 已配对管理 + 边缘映射
 * ④ 会话/控制状态。kvm.* 事件驱动刷新（host.module_state 转发契约）。
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
  sectionHead: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  row: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    flexWrap: "wrap",
  },
  code: {
    fontSize: tokens.fontSizeBase600,
    fontSpacing: "4px",
    letterSpacing: "6px",
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorBrandForeground1,
  },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  error: {
    color: tokens.colorPaletteRedForeground1,
    fontSize: tokens.fontSizeBase200,
  },
});

const fmtFp = (fp: string) => (fp.length > 16 ? `${fp.slice(0, 8)}…${fp.slice(-8)}` : fp);

export default function KvmPanel() {
  const styles = useStyles();
  const [pairCode, setPairCode] = useState<string>("");
  const [codeTtl, setCodeTtl] = useState<number>(0);
  const [discovered, setDiscovered] = useState<PeerInfoDto[]>([]);
  const [paired, setPaired] = useState<PairedPeerDto[]>([]);
  const [sessions, setSessions] = useState<SessionDto[]>([]);
  const [control, setControl] = useState<ControlStateDto>({ role: "idle" });
  const [edgeMap, setEdgeMapState] = useState<Record<string, string>>({});
  const [pairInput, setPairInput] = useState<Record<string, string>>({});
  const [error, setError] = useState<string>("");
  const [busy, setBusy] = useState<string>("");
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const [peers, pairs, sess, ctrl, edges] = await Promise.all([
        kvmDiscoveredPeers(),
        kvmPairedPeers(),
        kvmSessionList(),
        kvmControlState(),
        kvmEdgeMap(),
      ]);
      if (!mounted.current) return;
      setDiscovered(peers);
      setPaired(pairs);
      setSessions(sess);
      setControl(ctrl);
      setEdgeMapState(edges);
      setError("");
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    }
  }, []);

  // 初次加载 + kvm.* 事件驱动刷新（peer 上线/离线、配对、会话、控制权）
  useEffect(() => {
    mounted.current = true;
    void refresh();
    void kvmIssuePairCode()
      .then(([code, ttl]) => {
        if (mounted.current) {
          setPairCode(code);
          setCodeTtl(ttl);
        }
      })
      .catch(() => undefined);
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic ?? "";
          if (topic.startsWith("kvm.")) {
            void refresh();
            if (topic === "kvm.control_state") {
              void kvmControlState().then((c) => mounted.current && setControl(c));
            }
          }
        }),
      )
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
      });
    return () => {
      cancelled = true;
      mounted.current = false;
      unlisten?.();
    };
  }, [refresh]);

  // 一次性码倒计时自动续签
  useEffect(() => {
    if (!codeTtl) return;
    const t = window.setTimeout(
      () => {
        void kvmIssuePairCode()
          .then(([code, ttl]) => {
            setPairCode(code);
            setCodeTtl(ttl);
          })
          .catch(() => undefined);
      },
      Math.max(1000, codeTtl),
    );
    return () => window.clearTimeout(t);
  }, [pairCode, codeTtl]);

  const doPair = async (peer: PeerInfoDto) => {
    const code = (pairInput[peer.device_id] ?? "").trim();
    if (!code) {
      setError("请先输入对端显示的 6 位配对码");
      return;
    }
    setBusy(peer.device_id);
    try {
      await kvmPairWith(peer.addr, code);
      setError("");
      await refresh();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy("");
    }
  };

  const doConnect = async (device: PairedPeerDto) => {
    const peer = discovered.find((p) => p.device_id === device.device_id);
    if (!peer) {
      setError(`设备 ${device.device_name} 当前不在线，无法连接`);
      return;
    }
    setBusy(device.device_id);
    try {
      await kvmConnectTo(peer.addr);
      setError("");
      await refresh();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy("");
    }
  };

  const doUnpair = async (deviceId: string) => {
    try {
      await kvmUnpair(deviceId);
      setError("");
      await refresh();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const doEdgeChange = async (deviceId: string, edge: string) => {
    const next = { ...edgeMap, [deviceId]: edge };
    try {
      await kvmSetEdgeMap(next);
      setEdgeMapState(next);
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const onlineIds = new Set(discovered.map((p) => p.device_id));
  const sessionIds = new Set(sessions.map((s) => s.device_id));
  const unpaired = discovered.filter((p) => !paired.some((q) => q.device_id === p.device_id));

  return (
    <div className={styles.root}>
      {/* ① 控制状态 + 本端配对码 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">控制状态</Text>
          {control.role === "controlling" && (
            <Badge appearance="filled" color="brand">
              正在控制 {control.device_id}
            </Badge>
          )}
          {control.role === "controlled" && (
            <Badge appearance="filled" color="warning">
              被对端控制
            </Badge>
          )}
          {control.role === "idle" && <Badge appearance="outline">空闲</Badge>}
          {control.role === "controlling" && (
            <Button size="small" onClick={() => void kvmReleaseControl()}>
              切回本机
            </Button>
          )}
          <span className={styles.muted}>
            切回方式：鼠标移回对端共享边缘（优先）· Ctrl+Alt+Shift+Q
          </span>
        </div>
        <div className={styles.row}>
          <Text>本端配对码（对端输入用，2 分钟有效）：</Text>
          <span className={styles.code}>{pairCode || "——————"}</span>
        </div>
        <Text size={200} className={styles.muted}>
          配对流程：两端都打开键鼠共享页 → 一端点"配对"并输入另一端显示的 6 位码 → 双向指纹校验完成。
        </Text>
      </div>

      {error && <div className={styles.error}>{error}</div>}

      {/* ② 发现的未配对设备 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">发现的设备</Text>
          <Badge appearance="outline">{unpaired.length}</Badge>
          <Button size="small" appearance="subtle" onClick={() => void refresh()}>
            刷新
          </Button>
        </div>
        {unpaired.length === 0 ? (
          <Text size={200} className={styles.muted}>
            局域网内暂未发现未配对设备（对端需运行 NexusForge 且键鼠共享已启动）
          </Text>
        ) : (
          <Table size="small">
            <TableBody>
              {unpaired.map((p) => (
                <TableRow key={p.device_id}>
                  <TableCell>
                    <Text weight="semibold">{p.device_name}</Text>
                  </TableCell>
                  <TableCell>
                    <span className={styles.mono}>{p.addr}</span>
                  </TableCell>
                  <TableCell>
                    <Input
                      size="small"
                      placeholder="对端 6 位码"
                      value={pairInput[p.device_id] ?? ""}
                      onChange={(_, d) =>
                        setPairInput((m) => ({ ...m, [p.device_id]: d.value }))
                      }
                    />
                  </TableCell>
                  <TableCell>
                    <Button
                      size="small"
                      appearance="primary"
                      disabled={busy === p.device_id}
                      onClick={() => void doPair(p)}
                    >
                      {busy === p.device_id ? "配对中…" : "配对"}
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>

      {/* ③ 已配对设备：连接 / 边缘映射 / 解除 */}
      <div className={styles.section}>
        <div className={styles.sectionHead}>
          <Text weight="semibold">已配对设备</Text>
          <Badge appearance="outline">{paired.length}</Badge>
        </div>
        {paired.length === 0 ? (
          <Text size={200} className={styles.muted}>
            尚未配对任何设备
          </Text>
        ) : (
          <Table size="small">
            <TableHeader>
              <TableRow>
                <TableHeaderCell>设备</TableHeaderCell>
                <TableHeaderCell>指纹</TableHeaderCell>
                <TableHeaderCell>状态</TableHeaderCell>
                <TableHeaderCell>共享边缘</TableHeaderCell>
                <TableHeaderCell>操作</TableHeaderCell>
              </TableRow>
            </TableHeader>
            <TableBody>
              {paired.map((d) => (
                <TableRow key={d.device_id}>
                  <TableCell>
                    <Text weight="semibold">{d.device_name}</Text>
                  </TableCell>
                  <TableCell>
                    <span className={styles.mono}>{fmtFp(d.fingerprint)}</span>
                  </TableCell>
                  <TableCell>
                    {sessionIds.has(d.device_id) ? (
                      <Badge appearance="filled" color="success">
                        会话中
                      </Badge>
                    ) : onlineIds.has(d.device_id) ? (
                      <Badge appearance="outline">在线</Badge>
                    ) : (
                      <Badge appearance="outline" color="subtle">
                        离线
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell>
                    <Select
                      size="small"
                      value={edgeMap[d.device_id] ?? ""}
                      onChange={(_, d2) => void doEdgeChange(d.device_id, d2.value)}
                    >
                      <option value="">未设置</option>
                      <option value="left">本机左缘</option>
                      <option value="right">本机右缘</option>
                    </Select>
                  </TableCell>
                  <TableCell>
                    <div className={styles.row}>
                      <Button
                        size="small"
                        appearance="primary"
                        disabled={
                          busy === d.device_id ||
                          !onlineIds.has(d.device_id) ||
                          sessionIds.has(d.device_id)
                        }
                        onClick={() => void doConnect(d)}
                      >
                        {busy === d.device_id ? "连接中…" : "连接"}
                      </Button>
                      <Button size="small" onClick={() => void doUnpair(d.device_id)}>
                        解除配对
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>

      {/* ④ 边缘切换说明 */}
      <div className={styles.section}>
        <Text weight="semibold">边缘切换工作方式</Text>
        <Text size={200} className={styles.muted}>
          为设备设置"共享边缘"后（如 设备 B = 本机右缘），本机鼠标推到屏幕右缘即开始用键鼠控制
          B（本机输入被转发，B 端注入执行）；B 的鼠标移到它的左缘（回移）即切回本机，或随时按
          Ctrl+Alt+Shift+Q 切回。边缘映射会话建立后即时生效。
        </Text>
      </div>
    </div>
  );
}
