import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Dropdown,
  Option,
  Spinner,
  Dialog,
  DialogSurface,
  DialogBody,
  DialogTitle,
  DialogContent,
  DialogActions,
  Table,
  TableBody,
  TableCell,
  TableRow,
  TableHeader,
  TableHeaderCell,
} from "@fluentui/react-components";
import "@xterm/xterm/css/xterm.css";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  fileRemoteChmod,
  fileRemoteDrivers,
  parseAppError,
  termAck,
  termDockerContainers,
  termDockerLifecycle,
  termDockerLogs,
  termKill,
  termResize,
  termSessions,
  termSftpDownload,
  termSftpList,
  termSftpMkdir,
  termSftpRemove,
  termSftpRename,
  termSftpUpload,
  termSpawnLocal,
  termSpawnWsl,
  termSshConfigHosts,
  termSshConnect,
  termSshExec,
  termSshFingerprintAck,
  termSshForgetHost,
  termSshKnownHosts,
  termWslList,
  termWrite,
  type DockerContainerDto,
  type RemoteDriverDto,
  type SftpEntryDto,
  type SshAuthDto,
  type SshExecResultDto,
  type SshHostEntryDto,
  type SshJumpHopDto,
  type SshKnownHostDto,
  type TermSessionDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { keyActivate } from "../../a11y";
import Section from "../../components/Section";
import Tabs from "../../components/Tabs";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";
import ForwardSection from "./ForwardSection";

/**
 * 终端与运维面板（docs/impl/06 T1–T6，M11 v1）：
 * - T1 本地 ConPTY 会话（PowerShell / 自定义命令行）+ T5 WSL 分发
 * - T2 输出经 term.output 事件写入 xterm；ack 背压（累计字节回传）
 * - T3 SSH 会话（密码/密钥，TOFU 指纹自动记录，变更报错）+ SFTP 列表/上传/下载
 *   （T-B1-8：「已知主机」Dialog 列表+删除，host 串一律原样回传保 [h]:port 形状）
 * - T6 Docker：容器列表（手动刷新）/ 启停 / 日志 tail
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
  grow: { flex: 1, minWidth: "120px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
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
  termHost: {
    height: "420px",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: "#1b1b1b",
    padding: "6px",
  },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
    maxHeight: "320px",
    overflowY: "auto",
  },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  itemHover: { backgroundColor: tokens.colorNeutralBackground3Hover, cursor: "pointer" },
  logs: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
    maxHeight: "260px",
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
    backgroundColor: tokens.colorNeutralBackground2,
  },
});

export default function TerminalPanel() {
  const styles = useStyles();
  const [tab, setTab] = useState<"term" | "docker">("term");
  const [err, setErr] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const fail = useCallback((e: unknown) => setErr(parseAppError(e)?.data.message ?? String(e)), []);

  // ---- 会话 ----
  const [sessions, setSessions] = useState<TermSessionDto[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [wsl, setWsl] = useState<string[]>([]);
  // 新建本地：自定义命令行（空 = 默认 PowerShell）
  const [customShell, setCustomShell] = useState("");
  // SSH 表单
  const [sshHost, setSshHost] = useState("");
  const [sshPort, setSshPort] = useState("22");
  const [sshUser, setSshUser] = useState("root");
  const [sshPass, setSshPass] = useState("");
  const [sshKeyPath, setSshKeyPath] = useState("");
  const [useKey, setUseKey] = useState(false);
  // ~/.ssh/config 只读导入（T-B7-3）：懒载——下拉未开前零后端读（null=未加载）
  const [cfgHosts, setCfgHosts] = useState<SshHostEntryDto[] | null>(null);
  // ProxyJump 跳板（T-B7-4）：默认收起，展开前不采任何跳字段（collapsedByDefault 机检面）
  const [jumpOpen, setJumpOpen] = useState(false);
  const [jumpHost, setJumpHost] = useState("");
  const [jumpPort, setJumpPort] = useState("22");
  const [jumpUser, setJumpUser] = useState("");
  const [jumpPass, setJumpPass] = useState("");
  // 已知主机管理（T-B1-8）：host 串含 [h]:port 复合形态一律原样回传，
  // parse_host_port（commands/term.rs:195-203）依赖该形状，前端禁止拆解重组。
  const [khOpen, setKhOpen] = useState(false);
  const [khList, setKhList] = useState<SshKnownHostDto[] | null>(null);
  const [khBusy, setKhBusy] = useState(false);
  // 一次性远端命令（T-B7-2）：无 PTY exec，stdout/stderr/exit 三分区呈现
  // ——独立对话框结果区，不冒充终端回显
  const [execOpen, setExecOpen] = useState(false);
  const [execCmd, setExecCmd] = useState("");
  const [execBusy, setExecBusy] = useState(false);
  const [execResult, setExecResult] = useState<SshExecResultDto | null>(null);
  // 端口转发管理表（T-B7-5，红线批）：转发挂活 SSH 会话，独立对话框
  const [fwdOpen, setFwdOpen] = useState(false);
  // SFTP
  const [sftpPath, setSftpPath] = useState("/root");
  const [sftpEntries, setSftpEntries] = useState<SftpEntryDto[]>([]);
  const [sftpRemote, setSftpRemote] = useState("");
  const [sftpLocal, setSftpLocal] = useState("");
  const [sftpBusy, setSftpBusy] = useState(false);
  // T-B7-6 SFTP 变更操作（删除确认钮 / 新建目录 / 重命名）——stat 不上 UI，
  // 命令面供 file 域与后续消费；删除走二次确认（arm→再点执行）防误删
  const [sftpMkDir, setSftpMkDir] = useState("");
  const [sftpRenaming, setSftpRenaming] = useState<{ from: string; to: string } | null>(null);
  const [sftpDeleteArm, setSftpDeleteArm] = useState<string | null>(null);
  // T-B7-25 权限弹窗：写回**不**走 term-core（那里零 chmod）——宿主桥=直调
  // file 域 fileRemoteChmod 唯一口，目标驱动取自 file-core 已连接 SFTP 站点。
  // 两域会话互不共享是既有事实源形状，故这里选的是"哪条已连接远端"而非本终端会话。
  const [permFor, setPermFor] = useState<string | null>(null);
  const [permDrivers, setPermDrivers] = useState<RemoteDriverDto[] | null>(null);
  const [permDriver, setPermDriver] = useState("");
  const [permOctal, setPermOctal] = useState("");

  // ---- Docker ----
  const [containers, setContainers] = useState<DockerContainerDto[] | null>(null);
  const [dockerLoading, setDockerLoading] = useState(false);
  const [logsFor, setLogsFor] = useState<string | null>(null);
  const [logsText, setLogsText] = useState("");

  // xterm 实例表 + ack 计数（ref 避免重渲染）
  const termsRef = useRef<Map<string, { term: Terminal; fit: FitAddon; received: number }>>(new Map());
  const hostRef = useRef<HTMLDivElement | null>(null);

  const refreshSessions = useCallback(async () => {
    try {
      const list = await termSessions();
      setSessions(list);
    } catch (e) {
      reportError(e, { context: "终端会话列表刷新失败", dedupeKey: "term-sessions", toast: false });
    }
  }, []);

  const loadContainers = useCallback(async () => {
    setDockerLoading(true);
    setErr(null);
    try {
      setContainers(await termDockerContainers());
    } catch (e) {
      setContainers([]);
      fail(e);
    } finally {
      setDockerLoading(false);
    }
  }, [fail]);

  // 为会话创建 xterm 实例
  const ensureTerm = useCallback((s: TermSessionDto) => {
    let entry = termsRef.current.get(s.id);
    if (!entry) {
      const term = new Terminal({
        fontSize: 13,
        fontFamily: "Consolas, 'Courier New', monospace",
        cursorBlink: true,
        convertEol: false,
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      entry = { term, fit, received: 0 };
      termsRef.current.set(s.id, entry);
      // 输入 → 后端；会话方向键/控制序列全透传
      term.onData((data) => void termWrite(s.id, data));
      // 首次 resize 上报
      term.onResize((size) =>
        void termResize(s.id, size.cols, size.rows).catch((e) =>
          reportError(e, { context: "终端尺寸上报失败", dedupeKey: "term-resize", toast: false }),
        ),
      );
    }
    return entry;
  }, []);

  // 挂载激活会话的 xterm 到 DOM
  useEffect(() => {
    const host = hostRef.current;
    if (!active) return;
    const s = sessions.find((x) => x.id === active);
    if (!s || !host) return;
    const entry = ensureTerm(s);
    if (entry.term.element === undefined || !entry.term.element?.isConnected) {
      host.innerHTML = "";
      entry.term.open(host);
    }
    try {
      entry.fit.fit();
    } catch {
      /* 隐藏时忽略 */
    }
  }, [active, sessions, ensureTerm]);

  // 初始 + 事件驱动
  useEffect(() => {
    void refreshSessions();
    void termWslList()
      .then(setWsl)
      .catch((e) =>
        reportError(e, { context: "WSL 发行版列表加载失败", dedupeKey: "term-wsl-list", toast: false }),
      );
    if (!("__TAURI_INTERNALS__" in window)) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<{ topic: string; payload: { session_id: string; data?: string } }>("nf:event", (e) => {
          if (e.payload?.topic === "term.output") {
            const { session_id, data } = e.payload.payload;
            const entry = termsRef.current.get(session_id);
            if (entry && data) {
              entry.term.write(data);
              entry.received += data.length; // 近似字节数（UTF-16 差异可接受，ack 语义为吞吐反馈）
              void termAck(session_id, entry.received).catch((e) =>
                reportError(e, { context: "终端吞吐回执失败", dedupeKey: "term-ack", toast: false }),
              );
            }
          } else if (e.payload?.topic === "term.exit") {
            const sid = e.payload.payload.session_id;
            termsRef.current.get(sid)?.term.write("\r\n\x1b[90m[会话已结束]\x1b[0m\r\n");
            void refreshSessions();
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
  }, [refreshSessions]);

  const spawnLocal = useCallback(async () => {
    setErr(null);
    try {
      const s = await termSpawnLocal(
        customShell.trim() || undefined,
        undefined,
        100,
        26,
      );
      setSessions((prev) => [...prev, s]);
      setActive(s.id);
      setMsg(`已启动 ${s.title}`);
    } catch (e) {
      fail(e);
    }
  }, [customShell, fail]);

  const spawnWsl = useCallback(
    async (distro: string) => {
      setErr(null);
      try {
        const s = await termSpawnWsl(distro, 100, 26);
        setSessions((prev) => [...prev, s]);
        setActive(s.id);
        setMsg(`已启动 WSL: ${distro}`);
      } catch (e) {
        fail(e);
      }
    },
    [fail],
  );

  const sshAuth = useCallback((): SshAuthDto => {
    if (useKey) {
      return { kind: "key", key_path: sshKeyPath, passphrase: sshPass || undefined };
    }
    return { kind: "password", password: sshPass };
  }, [useKey, sshKeyPath, sshPass]);

  // 单跳 ProxyJump（T-B7-4）：未展开/主机空 = null（后端 jump=None 直连腿逐字不变）。
  // 凭据形状复用 SshAuthDto——与目标腿同一登记面
  const jumpHop = useCallback((): SshJumpHopDto | null => {
    if (!jumpOpen || !jumpHost.trim()) return null;
    return {
      host: jumpHost.trim(),
      port: Number(jumpPort) || 22,
      user: jumpUser,
      auth: { kind: "password", password: jumpPass },
      via: null,
    };
  }, [jumpOpen, jumpHost, jumpPort, jumpUser, jumpPass]);

  const spawnSsh = useCallback(async () => {
    if (!sshHost.trim()) return;
    setErr(null);
    const host = sshHost.trim();
    const port = Number(sshPort) || 22;
    const attempt = async () =>
      termSshConnect({ host, port, user: sshUser, auth: sshAuth(), jump: jumpHop(), cols: 100, rows: 26 });
    const openSession = (s: TermSessionDto) => {
      setSessions((prev) => [...prev, s]);
      setActive(s.id);
      setMsg(`SSH 已连接 ${s.title}`);
    };
    try {
      openSession(await attempt());
    } catch (e) {
      // T-B7-1 TOFU 首见臂：TERM_SSH_004 的 hint 携带整键描述符逐字——
      // 确认对话框展示的就是盘上要比对的那串；明示核对后才记信任并仅重连一次
      const err = parseAppError(e);
      if (err?.data.code === "TERM_SSH_004" && err.data.hint) {
        // 跳臂拒连（消息含"第 N 跳"）：指纹属于跳板主机，而确认对话框的
        // host/port 取自目标表单——拿目标的键去 ack 跳的指纹=错绑，此臂
        // 只如实报错（多跳逐跳确认通道人工核对，批次尾台账登记）
        if (/第 \d+ 跳/.test(err.data.message)) {
          fail(e);
          return;
        }
        const descriptor = err.data.hint;
        const ok = await confirmAction({
          title: "SSH 首见主机指纹确认",
          impact: [
            `主机 ${host}:${port} 尚无密钥记录`,
            `整键描述符（逐字）：${descriptor}`,
          ],
          detail:
            "请先经带外渠道与服务器侧核对这枚指纹完全一致，确认后才记入共享信任表并重连。心存疑虑就取消——本客户端没有隐式自纳这回事。",
          confirmLabel: "已核对，记录这枚指纹",
        });
        if (!ok) return;
        try {
          await termSshFingerprintAck(host, port, descriptor);
          openSession(await attempt());
        } catch (e2) {
          fail(e2);
        }
        return;
      }
      fail(e);
    }
  }, [sshHost, sshPort, sshUser, sshAuth, jumpHop, fail]);

  const killSession = useCallback(
    async (s: TermSessionDto) => {
      // 破坏性操作（D-18）：点名会话标题 + 连接类型，确认后立即杀进程
      const kind = s.kind;
      const kindDesc =
        kind.kind === "ssh"
          ? `SSH · ${kind.user}@${kind.host}:${kind.port}`
          : kind.kind === "wsl"
            ? `WSL · ${kind.distro}`
            : "本地终端";
      if (
        !(await confirmAction({
          title: "结束终端会话",
          impact: [`将结束会话「${s.title}」`, `类型：${kindDesc}`],
          detail: "进程将立即终止，未保存的会话内容丢失。",
          confirmLabel: "结束会话",
        }))
      )
        return;
      try {
        await termKill(s.id);
        termsRef.current.delete(s.id);
        await refreshSessions();
      } catch (e) {
        fail(e);
      }
    },
    [refreshSessions, fail],
  );

  const sftpTarget = useCallback(() => {
    // jump 一并带上：exec 臂消费（SshConnectDto 同形状）；SFTP 命令面维持直连腿，
    // sftpTargetArgs 只取四元——多出的键不外溢
    return {
      host: sshHost.trim(),
      port: Number(sshPort) || 22,
      user: sshUser,
      auth: sshAuth(),
      jump: jumpHop(),
    };
  }, [sshHost, sshPort, sshUser, sshAuth, jumpHop]);

  const loadSftp = useCallback(async () => {
    setSftpBusy(true);
    setErr(null);
    try {
      setSftpEntries(await termSftpList(...sftpTargetArgs(sftpTarget(), sftpPath)));
    } catch (e) {
      fail(e);
    } finally {
      setSftpBusy(false);
    }
  }, [sftpTarget, sftpPath, fail]);

  const joinRemote = useCallback(
    (name: string) => `${sftpPath.replace(/\/$/, "")}/${name}`,
    [sftpPath],
  );

  // T-B7-6 变更操作统一收束：成功即重取列表（结果永不"以为成了"——被拒的
  // 目录非空/禁覆盖都从后端回执点名，不静默）
  const sftpMutate = useCallback(
    async (run: () => Promise<unknown>, okMsg: string) => {
      setSftpBusy(true);
      setErr(null);
      try {
        await run();
        setMsg(okMsg);
        await loadSftp();
      } catch (e) {
        fail(e);
      } finally {
        setSftpBusy(false);
      }
    },
    [fail, loadSftp],
  );

  const sftpMkdir = useCallback(() => {
    const name = sftpMkDir.trim();
    if (!name) return;
    const t = sftpTarget();
    void sftpMutate(
      () => termSftpMkdir(t.host, t.port, t.user, t.auth, joinRemote(name)),
      `已创建目录 ${name}`,
    ).then(() => setSftpMkDir(""));
  }, [sftpMkDir, sftpTarget, sftpMutate, joinRemote]);

  const sftpRename = useCallback(() => {
    if (!sftpRenaming) return;
    const { from, to } = sftpRenaming;
    const t = sftpTarget();
    void sftpMutate(
      () => termSftpRename(t.host, t.port, t.user, t.auth, joinRemote(from), joinRemote(to)),
      `已重命名 ${from} → ${to}`,
    ).then(() => setSftpRenaming(null));
  }, [sftpRenaming, sftpTarget, sftpMutate, joinRemote]);

  const sftpRemove = useCallback(
    (name: string) => {
      const t = sftpTarget();
      void sftpMutate(
        () => termSftpRemove(t.host, t.port, t.user, t.auth, joinRemote(name)),
        `已删除 ${name}`,
      ).then(() => setSftpDeleteArm(null));
    },
    [sftpTarget, sftpMutate, joinRemote],
  );

  // T-B7-25 权限写回唯一通路：fileRemoteChmod（file-core 是全app唯一 chmod 口，
  // term-core 零第二份实现）。目标驱动只出自 file 域已连接远端表——本终端的
  // SSH 会话不是权限位的落点事实源，弹窗不假称"这条会话的权限"。
  const openPerm = useCallback(
    async (name: string) => {
      setPermFor(joinRemote(name));
      setPermOctal("");
      setErr(null);
      try {
        const list = (await fileRemoteDrivers()).filter((d) => d.protocol === "sftp");
        setPermDrivers(list);
        setPermDriver(list[0]?.driver_id ?? "");
      } catch (e) {
        setPermDrivers([]);
        fail(e);
      }
    },
    [joinRemote, fail],
  );

  const applyPerm = useCallback(async () => {
    if (!permFor || !permDriver) return;
    const text = permOctal.trim();
    const mode = /^[0-7]{1,4}$/.test(text) ? Number.parseInt(text, 8) : Number.NaN;
    if (Number.isNaN(mode)) {
      setErr("八进制权限位须为 1-4 位 0-7 数字（≤7777）");
      return;
    }
    setSftpBusy(true);
    setErr(null);
    try {
      await fileRemoteChmod(permDriver, permFor, mode);
      setMsg(`权限位已写回 ${permFor} → 0o${mode.toString(8)}`);
      setPermFor(null);
    } catch (e) {
      fail(e);
    } finally {
      setSftpBusy(false);
    }
  }, [permFor, permDriver, permOctal, fail]);

  const showLogs = useCallback(
    async (id: string) => {
      setErr(null);
      try {
        setLogsText(await termDockerLogs(id, 200));
        setLogsFor(id);
      } catch (e) {
        fail(e);
      }
    },
    [fail],
  );

  const lifecycle = useCallback(
    async (id: string, start: boolean) => {
      setErr(null);
      try {
        await termDockerLifecycle(id, start);
        await loadContainers();
      } catch (e) {
        fail(e);
      }
    },
    [loadContainers, fail],
  );

  // 停止容器（D-18）：中断容器内服务但可重启、数据卷保留 → 属可逆操作，danger=false
  const stopContainer = useCallback(
    async (c: DockerContainerDto) => {
      if (
        !(await confirmAction({
          title: "停止容器",
          impact: `将停止容器「${c.name}」`,
          detail: `镜像 ${c.image}；停止后容器内服务中断，数据卷保留，可随时「启动」恢复。`,
          danger: false,
          confirmLabel: "停止",
        }))
      )
        return;
      await lifecycle(c.id, false);
    },
    [lifecycle],
  );

  // ---- 已知主机（TOFU）管理（T-B1-8）----
  const loadKnownHosts = useCallback(async () => {
    try {
      setKhList(await termSshKnownHosts());
    } catch (e) {
      setKhList([]);
      fail(e);
    }
  }, [fail]);

  const forgetHost = useCallback(
    async (h: SshKnownHostDto) => {
      // B1 内唯一 danger 删除项：指纹信任记录删除不可逆，command 预览点名原样 host 串
      if (
        !(await confirmAction({
          title: "删除已知主机指纹",
          impact: `将删除「${h.host}」的 TOFU 指纹记录（1 条）`,
          detail:
            "删除后下次连接该主机将按首次连接重新记录指纹（TOFU）；" +
            "若对端并未重建密钥，重连时的指纹不符告警意味着中间人风险，请先核实。",
          command: h.host,
          danger: true,
          confirmLabel: "删除",
        }))
      )
        return;
      setKhBusy(true);
      try {
        const removed = await termSshForgetHost(h.host);
        setMsg(
          removed ? `已删除「${h.host}」的指纹记录` : `「${h.host}」未删除（可能已被移除）`,
        );
        await loadKnownHosts();
      } catch (e) {
        fail(e);
      } finally {
        setKhBusy(false);
      }
    },
    [fail, loadKnownHosts],
  );

  // 一次性非交互 exec（T-B7-2）：连接入参复用既有表单形状；结果三分区
  // 呈现，exit_code=null（未获退出码）与 0 严格两立，超时只认 timed_out
  const runExec = useCallback(async () => {
    if (!execCmd.trim()) return;
    setExecBusy(true);
    setErr(null);
    try {
      setExecResult(
        await termSshExec({ ...sftpTarget(), cols: 80, rows: 24 }, execCmd.trim()),
      );
    } catch (e) {
      fail(e);
    } finally {
      setExecBusy(false);
    }
  }, [execCmd, sftpTarget, fail]);

  // ---- ~/.ssh/config 只读导入（T-B7-3）----
  const loadCfgHosts = useCallback(async () => {
    try {
      setCfgHosts(await termSshConfigHosts());
    } catch (e) {
      setCfgHosts([]);
      fail(e);
    }
  }, [fail]);

  // 预填不是提交：只覆盖表单字段，用户仍可逐格改（host_name 缺席回落 alias）
  const applyCfg = useCallback(
    (alias: string) => {
      const h = cfgHosts?.find((x) => x.alias === alias);
      if (!h) return;
      setSshHost(h.host_name ?? h.alias);
      if (h.user) setSshUser(h.user);
      if (h.port != null) setSshPort(String(h.port));
      if (h.identity_file) {
        setSshKeyPath(h.identity_file);
        setUseKey(true);
      }
      setMsg(`已导入「${alias}」（仍可修改）`);
    },
    [cfgHosts],
  );

  return (
    <div className={styles.root}>
      <Tabs
        ariaLabel="终端视图"
        value={tab}
        onChange={setTab}
        items={[
          { id: "term", label: `终端（${sessions.filter((s) => s.alive).length} 活跃）` },
          { id: "docker", label: "Docker" },
        ]}
      />

      <InlineError text={msg} tone="success" />
      <InlineError text={err} />

      {tab === "term" && (
        <Section>
          <div className={styles.row}>
            <Input
              className={styles.grow}
              placeholder="自定义命令行（空 = 默认 PowerShell），如 wsl.exe -d Ubuntu"
              value={customShell}
              onChange={(_, d) => setCustomShell(d.value)}
            />
            <Button appearance="primary" size="small" onClick={() => void spawnLocal()}>
              新建本地终端
            </Button>
            <Dropdown placeholder="WSL 分发" value="" selectedOptions={[]} onOptionSelect={(_, d) => void spawnWsl(String(d.optionValue ?? ""))}>
              {wsl.map((d) => (
                <Option key={d} value={d} text={d}>
                  {d}
                </Option>
              ))}
              {wsl.length === 0 && <Option value="_none" text="（未检测到 WSL 分发）">（未检测到 WSL 分发）</Option>}
            </Dropdown>
          </div>

          <div className={styles.row}>
            <Input className={styles.grow} placeholder="SSH 主机" value={sshHost} onChange={(_, d) => setSshHost(d.value)} />
            <Input style={{ maxWidth: 80 }} placeholder="端口" value={sshPort} onChange={(_, d) => setSshPort(d.value)} />
            <Input style={{ maxWidth: 120 }} placeholder="用户" value={sshUser} onChange={(_, d) => setSshUser(d.value)} />
            <Dropdown
              placeholder="从 ~/.ssh/config 导入"
              value=""
              selectedOptions={[]}
              onOpenChange={() => {
                // 懒载：首次展开才读后端（未展开零调用），之后再开用缓存
                if (cfgHosts === null) void loadCfgHosts();
              }}
              onOptionSelect={(_, d) => applyCfg(String(d.optionValue ?? ""))}
            >
              {(cfgHosts ?? []).map((h) => (
                <Option key={h.alias} value={h.alias} text={h.alias}>
                  {h.alias}
                </Option>
              ))}
              {cfgHosts !== null && cfgHosts.length === 0 && (
                <Option value="_none" text="（无可导入主机）">（无可导入主机）</Option>
              )}
            </Dropdown>
            <Button
              size="small"
              appearance="outline"
              onClick={() => setUseKey((k) => !k)}
            >
              {useKey ? "密钥模式" : "密码模式"}
            </Button>
            {useKey ? (
              <Input className={styles.grow} placeholder="私钥路径（如 C:\\Users\\me\\.ssh\\id_ed25519）" value={sshKeyPath} onChange={(_, d) => setSshKeyPath(d.value)} />
            ) : null}
            <Input className={styles.grow} placeholder={useKey ? "密钥口令（可空）" : "密码"} type="password" value={sshPass} onChange={(_, d) => setSshPass(d.value)} />
            <Button size="small" appearance="primary" onClick={() => void spawnSsh()}>
              SSH 连接
            </Button>
            <Button
              size="small"
              appearance="outline"
              title="查看并管理 SSH 首次连接记录的 TOFU 主机指纹"
              onClick={() => {
                setKhOpen(true);
                void loadKnownHosts();
              }}
            >
              已知主机
            </Button>
            <Button
              size="small"
              appearance="outline"
              title="一次性非交互远端命令（exec，无 PTY；结果三分区呈现，不进终端回显）"
              onClick={() => {
                setExecResult(null);
                setExecOpen(true);
              }}
            >
              一次性远端命令
            </Button>
            <Button
              size="small"
              appearance="outline"
              title="端口转发 -L/-R/-D 管理表（转发挂 SSH 会话、会话关即全拆；端口占用显示被拒行而非静默）"
              onClick={() => setFwdOpen(true)}
            >
              端口转发
            </Button>
            {/* T-B7-28（§7.3-a 明示不做）：term 是 per-tab 长会话、file 是 per-op 短连接，
                两域共用一条 SSH 会话需生命周期仲裁者，属 B8 待裁决——以徽标诚实登记。 */}
            <DeferredBadge label="SSH 连接池统一" decisionRef="09 §7.3-(a)" />
          </div>

          <div className={styles.row}>
            {/* ProxyJump 跳板（T-B7-4）：默认收起——收起态不渲染任何跳字段 */}
            <Button
              size="small"
              appearance="outline"
              title="经一台跳板机中转连接目标（逐跳独立 TOFU 核验；深度上限 3 跳）"
              aria-expanded={jumpOpen}
              onClick={() => setJumpOpen((o) => !o)}
            >
              {jumpOpen ? "收起 ProxyJump 跳板" : "ProxyJump 跳板"}
            </Button>
            {jumpOpen ? (
              <>
                <Input
                  className={styles.grow}
                  placeholder="跳板主机（单跳，密码凭据）"
                  value={jumpHost}
                  onChange={(_, d) => setJumpHost(d.value)}
                />
                <Input
                  style={{ maxWidth: 80 }}
                  placeholder="跳板端口"
                  value={jumpPort}
                  onChange={(_, d) => setJumpPort(d.value)}
                />
                <Input
                  style={{ maxWidth: 120 }}
                  placeholder="跳板用户"
                  value={jumpUser}
                  onChange={(_, d) => setJumpUser(d.value)}
                />
                <Input
                  className={styles.grow}
                  placeholder="跳板密码"
                  type="password"
                  value={jumpPass}
                  onChange={(_, d) => setJumpPass(d.value)}
                />
              </>
            ) : null}
          </div>

          <div className={styles.row}>
            {sessions.map((s) => (
              <button
                key={s.id}
                className={`${styles.tab} ${active === s.id ? styles.tabActive : ""}`}
                onClick={() => setActive(s.id)}
              >
                {s.title}
                {!s.alive && <Badge size="small" appearance="ghost">结束</Badge>}
                <span
                  style={{ marginLeft: 4, color: tokens.colorNeutralForeground3 }}
                  onClick={(e) => {
                    e.stopPropagation();
                    void killSession(s);
                  }}
                  role="button"
                  tabIndex={0}
                  onKeyDown={keyActivate(() => void killSession(s))}
                >
                  ✕
                </span>
              </button>
            ))}
          </div>

          <div ref={hostRef} className={styles.termHost} style={{ display: active ? "block" : "none" }} />
          {!active && <Text className={styles.muted}>新建或选择一个会话（本地 PowerShell / WSL / SSH）</Text>}

          {active && (
            <div className={styles.row}>
              <Input className={styles.grow} placeholder="SFTP 远程路径" value={sftpPath} onChange={(_, d) => setSftpPath(d.value)} />
              <Button size="small" onClick={() => void loadSftp()}>
                SFTP 浏览
              </Button>
              {sftpBusy && <Spinner size="tiny" />}
            </div>
          )}
          {active && (
            <div className={styles.row}>
              <Input
                className={styles.grow}
                placeholder="新目录名（当前路径下）"
                value={sftpMkDir}
                onChange={(_, d) => setSftpMkDir(d.value)}
              />
              <Button size="small" disabled={!sftpMkDir.trim() || sftpBusy} onClick={sftpMkdir}>
                新建目录
              </Button>
            </div>
          )}
          {sftpEntries.length > 0 && (
            <div className={styles.list}>
              {sftpEntries.map((e) => (
                <div
                  key={e.name}
                  className={`${styles.item} ${styles.itemHover}`}
                  onClick={() => {
                    const next = `${sftpPath.replace(/\/$/, "")}/${e.name}`;
                    setSftpPath(next);
                    if (e.is_dir) void loadSftp();
                  }}
                  role="button"
                  tabIndex={0}
                  onKeyDown={keyActivate(() => {
                    const next = `${sftpPath.replace(/\/$/, "")}/${e.name}`;
                    setSftpPath(next);
                    if (e.is_dir) void loadSftp();
                  })}
                >
                  {sftpRenaming?.from === e.name ? (
                    <span
                      style={{ display: "flex", gap: 4, alignItems: "center", width: "100%" }}
                    >
                      <Input
                        className={styles.grow}
                        placeholder="新名（目标存在则拒，不覆盖）"
                        value={sftpRenaming.to}
                        onClick={(ev) => ev.stopPropagation()}
                        onChange={(_, d) => setSftpRenaming({ from: e.name, to: d.value })}
                      />
                      <Button
                        size="small"
                        disabled={!sftpRenaming.to.trim() || sftpBusy}
                        onClick={(ev) => {
                          ev.stopPropagation();
                          sftpRename();
                        }}
                      >
                        确认
                      </Button>
                      <Button
                        size="small"
                        appearance="subtle"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          setSftpRenaming(null);
                        }}
                      >
                        取消
                      </Button>
                    </span>
                  ) : (
                    <>
                      <Text size={200}>{e.is_dir ? "📁" : "📄"} {e.name}</Text>
                      {!e.is_dir && (
                        <Text size={100} className={styles.muted}>
                          {(e.size / 1024).toFixed(1)} KB
                        </Text>
                      )}
                      <span style={{ marginLeft: "auto", display: "flex", gap: 2 }}>
                        <Button
                          size="small"
                          appearance="subtle"
                          disabled={sftpBusy}
                          onClick={(ev) => {
                            ev.stopPropagation();
                            setSftpRenaming({ from: e.name, to: e.name });
                          }}
                        >
                          重命名
                        </Button>
                        {sftpDeleteArm === e.name ? (
                          <>
                            <Button
                              size="small"
                              appearance="outline"
                              disabled={sftpBusy}
                              onClick={(ev) => {
                                ev.stopPropagation();
                                sftpRemove(e.name);
                              }}
                            >
                              确认删除
                            </Button>
                            <Button
                              size="small"
                              appearance="subtle"
                              onClick={(ev) => {
                                ev.stopPropagation();
                                setSftpDeleteArm(null);
                              }}
                            >
                              取消
                            </Button>
                          </>
                        ) : (
                          <Button
                            size="small"
                            appearance="subtle"
                            disabled={sftpBusy}
                            onClick={(ev) => {
                              ev.stopPropagation();
                              setSftpDeleteArm(e.name);
                            }}
                          >
                            删除
                          </Button>
                        )}
                        <Button
                          size="small"
                          appearance="subtle"
                          onClick={(ev) => {
                            ev.stopPropagation();
                            void openPerm(e.name);
                          }}
                        >
                          权限
                        </Button>
                      </span>
                    </>
                  )}
                </div>
              ))}
            </div>
          )}
          {active && sftpRemote !== null && (
            <div className={styles.row}>
              <Input className={styles.grow} placeholder="远程文件路径（上传/下载目标）" value={sftpRemote} onChange={(_, d) => setSftpRemote(d.value)} />
              <Input className={styles.grow} placeholder="本地文件路径" value={sftpLocal} onChange={(_, d) => setSftpLocal(d.value)} />
              <Button
                size="small"
                onClick={() =>
                  void termSftpDownload(...sftpDlArgs(sftpTarget(), sftpRemote, sftpLocal))
                    .then((n) => setMsg(`已下载 ${n} 字节`))
                    .catch(fail)
                }
              >
                下载
              </Button>
              <Button
                size="small"
                onClick={() =>
                  void termSftpUpload(...sftpUlArgs(sftpTarget(), sftpLocal, sftpRemote))
                    .then((n) => setMsg(`已上传 ${n} 字节`))
                    .catch(fail)
                }
              >
                上传
              </Button>
            </div>
          )}
          {permFor && (
            <Dialog open onOpenChange={(_, d) => !d.open && setPermFor(null)}>
              <DialogSurface>
                <DialogBody>
                  <DialogTitle>远端权限位</DialogTitle>
                  <DialogContent>
                    <div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
                      <Text size={200}>目标 {permFor}</Text>
                      {permDrivers && permDrivers.length === 0 && (
                        <Text size={200} className={styles.muted}>
                          无已连接的 SFTP 远端：权限写回只认文件域『连接』档建立的
                          驱动事实源，请先在那里连接同站点
                        </Text>
                      )}
                      {permDrivers && permDrivers.length > 0 && (
                        <Dropdown
                          value={permDrivers.find((d) => d.driver_id === permDriver)?.label ?? ""}
                          selectedOptions={[permDriver]}
                          onOptionSelect={(_, o) => setPermDriver(o.optionValue ?? "")}
                        >
                          {permDrivers.map((d) => (
                            <Option key={d.driver_id} value={d.driver_id} text={d.label}>
                              {d.label}（{d.driver_id}）
                            </Option>
                          ))}
                        </Dropdown>
                      )}
                      {permDrivers && permDrivers.length > 0 && (
                        <div className={styles.row}>
                          <Input
                            className={styles.grow}
                            placeholder="新八进制权限位（如 644 / 7777）"
                            value={permOctal}
                            onChange={(_, d) => setPermOctal(d.value)}
                          />
                          <Button size="small" disabled={sftpBusy} onClick={() => void applyPerm()}>
                            写回
                          </Button>
                        </div>
                      )}
                    </div>
                  </DialogContent>
                  <DialogActions>
                    <Button appearance="subtle" onClick={() => setPermFor(null)}>
                      关闭
                    </Button>
                  </DialogActions>
                </DialogBody>
              </DialogSurface>
            </Dialog>
          )}
        </Section>
      )}

      {tab === "docker" && (
        <Section
          title="容器（Docker Desktop 需运行中）"
          actions={
            <Button size="small" onClick={() => void loadContainers()}>
              刷新
            </Button>
          }
        >
          {dockerLoading && <Spinner size="tiny" />}
          {containers && containers.length === 0 && !dockerLoading && (
            <EmptyState text="无容器（或 Docker Engine 不可达）" />
          )}
          <div className={styles.list}>
            {(containers ?? []).map((c) => (
              <div key={c.id} className={styles.item}>
                <Badge appearance={c.state === "running" ? "filled" : "outline"} color={c.state === "running" ? "success" : "subtle"}>
                  {c.state}
                </Badge>
                <Text size={200} weight="semibold">
                  {c.name}
                </Text>
                <Text size={100} className={styles.muted}>
                  {c.image} · {c.status}
                </Text>
                <div className={styles.grow} />
                {c.state === "running" ? (
                  <Button size="small" appearance="subtle" onClick={() => void stopContainer(c)}>
                    停止
                  </Button>
                ) : (
                  <Button size="small" appearance="subtle" onClick={() => void lifecycle(c.id, true)}>
                    启动
                  </Button>
                )}
                <Button size="small" appearance="subtle" onClick={() => void showLogs(c.id)}>
                  日志
                </Button>
              </div>
            ))}
          </div>
          {logsFor && (
            <div>
              <div className={styles.row}>
                <Text size={200} weight="semibold">
                  日志：{logsFor}
                </Text>
                <div className={styles.grow} />
                <Button size="small" appearance="subtle" onClick={() => setLogsFor(null)}>
                  关闭
                </Button>
              </div>
              <div className={styles.logs}>{logsText || "（无输出）"}</div>
            </div>
          )}
        </Section>
      )}

      {/* 已知主机（TOFU）管理 Dialog（T-B1-8）：host 列原样展示原样回传 */}
      <Dialog
        open={khOpen}
        onOpenChange={(_, d) => {
          if (!d.open) setKhOpen(false);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>已知主机（SSH 指纹管理）</DialogTitle>
            <DialogContent>
              {khList === null ? (
                <EmptyState text="指纹列表加载中" loading />
              ) : khList.length === 0 ? (
                <EmptyState text="暂无已知主机（SSH 首次连接确认后自动记录）" />
              ) : (
                <Table size="small">
                  <TableHeader>
                    <TableRow>
                      <TableHeaderCell>主机</TableHeaderCell>
                      <TableHeaderCell>指纹</TableHeaderCell>
                      <TableHeaderCell>操作</TableHeaderCell>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {khList.map((h) => (
                      <TableRow key={h.host}>
                        <TableCell>
                          <span className={styles.muted}>{h.host}</span>
                        </TableCell>
                        <TableCell>
                          <span className={styles.muted}>{h.fingerprint}</span>
                        </TableCell>
                        <TableCell>
                          <Button
                            size="small"
                            disabled={khBusy}
                            onClick={() => void forgetHost(h)}
                          >
                            删除
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setKhOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
      {/* 端口转发管理表 Dialog（T-B7-5，红线批）：每行真 state 徽标，
          端口占用显示被拒行而非空表；非回环绑定须地址+确认位齐备（bind_gate UI 镜像） */}
      <Dialog
        open={fwdOpen}
        onOpenChange={(_, d) => {
          if (!d.open) setFwdOpen(false);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>端口转发（-L 本地 / -R 远端 / -D SOCKS5）</DialogTitle>
            <DialogContent>
              <ForwardSection sessions={sessions} activeId={active} />
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setFwdOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
      {/* 一次性远端命令 Dialog（T-B7-2）：exit 徽标+stdout/stderr 三分区，
          结果永不回灌终端——exec 不是 shell 会话 */}
      <Dialog
        open={execOpen}
        onOpenChange={(_, d) => {
          if (!d.open) setExecOpen(false);
        }}
      >
        <DialogSurface>
          <DialogBody>
            <DialogTitle>一次性远端命令（exec，无 PTY）</DialogTitle>
            <DialogContent>
              <div className={styles.row}>
                <Input
                  className={styles.grow}
                  placeholder="非交互命令，如 hostname -s"
                  value={execCmd}
                  onChange={(_, d) => setExecCmd(d.value)}
                />
                <Button
                  size="small"
                  appearance="primary"
                  disabled={execBusy || !execCmd.trim()}
                  onClick={() => void runExec()}
                >
                  执行
                </Button>
                {execBusy && <Spinner size="tiny" />}
              </div>
              {execResult && (
                <div>
                  <span
                    data-exit-tone={
                      execResult.timed_out || (execResult.exit_code !== null && execResult.exit_code !== 0)
                        ? "danger"
                        : execResult.exit_code === 0
                          ? "ok"
                          : "none"
                    }
                  >
                    <Badge
                      size="small"
                      appearance="filled"
                      color={
                        execResult.timed_out ||
                        (execResult.exit_code !== null && execResult.exit_code !== 0)
                          ? "danger"
                          : execResult.exit_code === 0
                            ? "success"
                            : "subtle"
                      }
                    >
                      {execResult.timed_out
                        ? "超时（终态以 timed_out 为准）"
                        : execResult.exit_code === null
                          ? "未获退出码"
                          : `退出码 ${execResult.exit_code}`}
                    </Badge>
                  </span>
                  <Text size={200} weight="semibold">
                    stdout
                  </Text>
                  <div className={styles.logs}>{execResult.stdout || "（空）"}</div>
                  <Text size={200} weight="semibold">
                    stderr
                  </Text>
                  <div className={styles.logs}>{execResult.stderr || "（空）"}</div>
                </div>
              )}
              {!execResult && !execBusy && (
                <EmptyState text="执行后按 stdout / stderr / 退出码三分区呈现（不冒充终端回显）" />
              )}
            </DialogContent>
            <DialogActions>
              <Button appearance="subtle" onClick={() => setExecOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}

/** sftp 参数展开（保持调用处紧凑） */
function sftpTargetArgs(
  t: { host: string; port: number; user: string; auth: SshAuthDto },
  path: string,
): [string, number, string, SshAuthDto, string] {
  return [t.host, t.port, t.user, t.auth, path];
}
function sftpDlArgs(
  t: { host: string; port: number; user: string; auth: SshAuthDto },
  remote: string,
  local: string,
): [string, number, string, SshAuthDto, string, string] {
  return [t.host, t.port, t.user, t.auth, remote, local];
}
function sftpUlArgs(
  t: { host: string; port: number; user: string; auth: SshAuthDto },
  local: string,
  remote: string,
): [string, number, string, SshAuthDto, string, string] {
  return [t.host, t.port, t.user, t.auth, local, remote];
}
