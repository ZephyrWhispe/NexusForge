import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Select,
  Dialog,
  DialogSurface,
  DialogBody,
  DialogTitle,
  DialogContent,
  DialogActions,
  Textarea,
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
  kvmSendClip,
  kvmSendFile,
  kvmSessionList,
  kvmSetEdgeMap,
  kvmUnpair,
  parseAppError,
  type ControlStateDto,
  type PairedPeerDto,
  type PeerInfoDto,
  type SessionDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 键鼠共享面板（docs/impl/05 K8，M4 v1）：
 * ① 本端一次性码展示（对端输入用）② 发现设备配对 ③ 已配对管理 + 边缘映射
 * ④ 会话/控制状态。kvm.* 事件驱动刷新（host.module_state 转发契约）。
 * T-B1-7：推送剪贴板/推送文件仅对 role=client 的出站会话开放（核账⑤：
 * 后端 session_to 两种角色均可解析，客户端门禁是产品语义——server 会话是
 * 对端在控制本机，不是推送目标）；「活跃会话」卡按角色列出全部会话。
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
  row: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    flexWrap: "wrap",
  },
  code: {
    fontSize: tokens.fontSizeBase600,
    letterSpacing: "6px",
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorBrandForeground1,
  },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
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
  // 首轮刷新是否落定：未落定前两个设备列表渲染加载态而非"暂无"文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  // 推送回落对话框（readText 被拒）与文件路径对话框（T-B1-7）
  const [pushTextFor, setPushTextFor] = useState<PairedPeerDto | null>(null);
  const [pushTextValue, setPushTextValue] = useState("");
  const [pushTextBusy, setPushTextBusy] = useState(false);
  const [fileFor, setFileFor] = useState<PairedPeerDto | null>(null);
  const [filePath, setFilePath] = useState("");
  const [fileBusy, setFileBusy] = useState(false);
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
    } finally {
      if (mounted.current) setLoaded(true);
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
      .catch((e) => reportError(e, { context: "配对码签发失败", dedupeKey: "kvm-pair-code" }));
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
          .catch((e) =>
            reportError(e, { context: "配对码续签失败", dedupeKey: "kvm-pair-code", toast: false }),
          );
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

  // 解除配对（D-18）：删除双向凭据属破坏性操作，单击即改 → 全局确认框点名设备
  const doUnpair = async (device: PairedPeerDto) => {
    const inSession = sessions.some((s) => s.device_id === device.device_id);
    if (
      !(await confirmAction({
        title: "解除设备配对",
        impact: [
          `将解除与「${device.device_name}」的配对（1 台设备）`,
          `设备 ID ${device.device_id} · 指纹 ${fmtFp(device.fingerprint)}`,
          `该设备的共享边缘设置 ${edgeMap[device.device_id] ? "将一并失效" : "未设置"}`,
        ],
        detail:
          `双向凭据与指纹信任将从本机配对库中删除，${inSession ? "进行中的键鼠会话同时中断；" : ""}` +
          "对端也需重新走一次性码流程才能再次配对（本端码不影响）。",
        confirmLabel: "解除配对",
      }))
    )
      return;
    try {
      await kvmUnpair(device.device_id);
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

  const hasClientSession = (deviceId: string) =>
    sessions.some((s) => s.device_id === deviceId && s.role === "client");

  const notAllowedError = () => {
    const e = new Error("clipboard unavailable");
    e.name = "NotAllowedError";
    return e;
  };

  // 推送剪贴板（T-B1-7）：首径 navigator.clipboard.readText()；被拒（NotAllowedError）
  // 或 API 缺失 → 回落「推送文本」对话框（用户 Ctrl+V，无需任何权限）。两路都真 invoke。
  const doPushClip = async (device: PairedPeerDto) => {
    if (!hasClientSession(device.device_id)) return;
    setBusy(device.device_id);
    try {
      const readText = navigator.clipboard?.readText?.bind(navigator.clipboard);
      if (!readText) throw notAllowedError();
      const text = await readText();
      if (!text) {
        setError("本地剪贴板没有文本内容，未推送");
        return;
      }
      await kvmSendClip(device.device_id, { Text: { text, html: null } });
      setError("");
      notify("success", `已推送剪贴板文本到「${device.device_name}」`, "对端将收到并写入其剪贴板");
    } catch (e) {
      if ((e as { name?: string } | null)?.name === "NotAllowedError") {
        setPushTextFor(device);
        setPushTextValue("");
      } else {
        setError(parseAppError(e)?.data.message ?? String(e));
      }
    } finally {
      setBusy("");
    }
  };

  const doPushTextSend = async () => {
    const device = pushTextFor;
    const text = pushTextValue;
    if (!device || !text) return;
    setPushTextBusy(true);
    try {
      await kvmSendClip(device.device_id, { Text: { text, html: null } });
      setError("");
      setPushTextFor(null);
      notify("success", `已推送文本到「${device.device_name}」`, "对端将收到并写入其剪贴板");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setPushTextBusy(false);
    }
  };

  const doFileSend = async () => {
    const device = fileFor;
    const path = filePath.trim();
    if (!device || !path) return;
    if (
      !(await confirmAction({
        title: "发送文件到对端",
        impact: [
          `将向「${device.device_name}」发送 1 个文件`,
          `路径 ${path}`,
          "对端将接收并落盘，文件内容对对方可见",
        ],
        detail:
          "仅发送单个已存在文件路径（目录与通配符不会展开，多个文件请逐次发送）；" +
          "传输在后台进行，进度与回执经会话事件回报。",
        confirmLabel: "发送",
      }))
    )
      return;
    setFileBusy(true);
    try {
      await kvmSendFile(device.device_id, path);
      setError("");
      setFileFor(null);
      notify("success", `已开始发送文件到「${device.device_name}」`, "进度与回执经会话事件回报");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setFileBusy(false);
    }
  };

  const onlineIds = new Set(discovered.map((p) => p.device_id));
  // sessionIds 保持任意角色语义（「会话中」徽标 + 连接互斥）；推送门禁只看 client 出站会话
  const sessionIds = new Set(sessions.map((s) => s.device_id));
  const clientSessionIds = new Set(
    sessions.filter((s) => s.role === "client").map((s) => s.device_id),
  );
  const unpaired = discovered.filter((p) => !paired.some((q) => q.device_id === p.device_id));

  return (
    <div className={styles.root}>
      {/* ① 控制状态 + 本端配对码 */}
      <Section
        title="控制状态"
        actions={
          <>
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
          </>
        }
      >
        <span className={styles.muted}>
          切回方式：鼠标移回对端共享边缘（优先）· Ctrl+Alt+Shift+Q
        </span>
        <div className={styles.row}>
          <Text>本端配对码（对端输入用，2 分钟有效）：</Text>
          <span className={styles.code}>{pairCode || "——————"}</span>
        </div>
        <Text size={200} className={styles.muted}>
          配对流程：两端都打开键鼠共享页 → 一端点"配对"并输入另一端显示的 6 位码 → 双向指纹校验完成。
        </Text>
      </Section>

      <InlineError text={error} />

      {/* ② 发现的未配对设备 */}
      <Section
        title="发现的设备"
        actions={
          <>
            <Badge appearance="outline">{unpaired.length}</Badge>
            <Button size="small" appearance="subtle" onClick={() => void refresh()}>
              刷新
            </Button>
          </>
        }
      >
        {unpaired.length === 0 ? (
          <EmptyState
            text="局域网内暂未发现未配对设备（对端需运行 NexusForge 且键鼠共享已启动）"
            loading={!loaded}
          />
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
      </Section>

      {/* ③ 已配对设备：连接 / 边缘映射 / 解除 */}
      <Section
        title="已配对设备"
        actions={<Badge appearance="outline">{paired.length}</Badge>}
      >
        {paired.length === 0 ? (
          <EmptyState
            text="尚未配对任何设备"
            loading={!loaded}
          />
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
                      <option value="up">本机顶缘</option>
                      <option value="down">本机底缘</option>
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
                      <Button
                        size="small"
                        disabled={!clientSessionIds.has(d.device_id)}
                        title={
                          clientSessionIds.has(d.device_id)
                            ? "推送本机剪贴板文本（被拒时可手动输入）"
                            : "需先点「连接」建立本端发起的出站会话（对端接入的会话不视为推送目标）"
                        }
                        onClick={() => void doPushClip(d)}
                      >
                        推送剪贴板
                      </Button>
                      <Button
                        size="small"
                        disabled={!clientSessionIds.has(d.device_id)}
                        title={
                          clientSessionIds.has(d.device_id)
                            ? "发送单个本地文件到对端"
                            : "需先点「连接」建立本端发起的出站会话（对端接入的会话不视为推送目标）"
                        }
                        onClick={() => {
                          if (!clientSessionIds.has(d.device_id)) return;
                          setFileFor(d);
                          setFilePath("");
                        }}
                      >
                        推送文件
                      </Button>
                      <Button size="small" onClick={() => void doUnpair(d)}>
                        解除配对
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </Section>

      {/* ④ 活跃会话（T-B1-7）：按角色列出，client=本端发起（可推送），server=对端接入 */}
      <Section
        title="活跃会话"
        actions={<Badge appearance="outline">{sessions.length}</Badge>}
      >
        {sessions.length === 0 ? (
          <EmptyState
            text="当前无活跃会话（点已配对设备的「连接」发起，或等待对端接入）"
            loading={!loaded}
          />
        ) : (
          <Table size="small">
            <TableHeader>
              <TableRow>
                <TableHeaderCell>设备</TableHeaderCell>
                <TableHeaderCell>角色</TableHeaderCell>
              </TableRow>
            </TableHeader>
            <TableBody>
              {sessions.map((s) => (
                <TableRow key={`${s.role}:${s.device_id}`}>
                  <TableCell>
                    <Text weight="semibold">{s.device_name}</Text>
                  </TableCell>
                  <TableCell>
                    {s.role === "client" ? (
                      <Badge appearance="filled" color="brand">
                        client · 本端发起（可推送）
                      </Badge>
                    ) : (
                      <Badge appearance="filled" color="warning">
                        server · 对端接入
                      </Badge>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </Section>

      {/* ⑤ 边缘切换说明 */}
      <Section title="边缘切换工作方式">
        <Text size={200} className={styles.muted}>
          为设备设置"共享边缘"后（如 设备 B = 本机右缘），本机鼠标推到屏幕右缘即开始用键鼠控制
          B（本机输入被转发，B 端注入执行）；B 的鼠标移到它的左缘（回移）即切回本机，或随时按
          Ctrl+Alt+Shift+Q 切回。边缘映射会话建立后即时生效。
        </Text>
        {/* T-B7-28（§7.3-(b) 明示不做）：跨机文件推送已有按钮/右键口；
            从桌面把文件丢过边缘传送需宿主 shell 级落点，风险面大，归 B8 待裁决。 */}
        <DeferredBadge label="拖拽传文件" decisionRef="09 §7.3-(b)" />
      </Section>

      {/* 推送文本回落对话框：readText 被拒时手动输入/粘贴（无需剪贴板权限） */}
      <Dialog
        open={pushTextFor !== null}
        onOpenChange={(_, d) => {
          if (!d.open) setPushTextFor(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>推送文本到「{pushTextFor?.device_name ?? ""}」</DialogTitle>
            <DialogContent>
              <Text size={200} className={styles.muted}>
                自动读取本机剪贴板被系统拒绝，已回落手动模式：在下方输入或 Ctrl+V
                粘贴要推送的文本（此路径不需要任何剪贴板权限）。
              </Text>
              <Textarea
                rows={5}
                value={pushTextValue}
                onChange={(_, d) => setPushTextValue(d.value)}
                placeholder="要推送到对端剪贴板的文本"
              />
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setPushTextFor(null)}>
                取消
              </Button>
              <Button
                appearance="primary"
                disabled={!pushTextValue.trim() || pushTextBusy}
                onClick={() => void doPushTextSend()}
              >
                {pushTextBusy ? "推送中…" : "推送"}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* 推送文件对话框：单个已存在文件路径 + D-18 确认（跨机可见内容） */}
      <Dialog
        open={fileFor !== null}
        onOpenChange={(_, d) => {
          if (!d.open) setFileFor(null);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>发送文件到「{fileFor?.device_name ?? ""}」</DialogTitle>
            <DialogContent>
              <Text size={200} className={styles.muted}>
                仅发送单个已存在文件路径（目录与通配符不会展开）。
              </Text>
              <Input
                value={filePath}
                onChange={(_, d) => setFilePath(d.value)}
                placeholder="例如 C:\Users\me\Downloads\report.pdf"
              />
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setFileFor(null)}>
                取消
              </Button>
              <Button
                appearance="primary"
                disabled={!filePath.trim() || fileBusy}
                onClick={() => void doFileSend()}
              >
                {fileBusy ? "发送中…" : "发送"}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}
