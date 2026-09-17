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
} from "@fluentui/react-components";
import "@xterm/xterm/css/xterm.css";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import {
  termAck,
  termDockerContainers,
  termDockerLifecycle,
  termDockerLogs,
  termKill,
  termResize,
  termSessions,
  termSftpDownload,
  termSftpList,
  termSftpUpload,
  termSpawnLocal,
  termSpawnWsl,
  termSshConnect,
  termWslList,
  termWrite,
  parseAppError,
  type DockerContainerDto,
  type SftpEntryDto,
  type SshAuthDto,
  type TermSessionDto,
} from "../../ipc/client";

/**
 * 终端与运维面板（docs/impl/06 T1–T6，M11 v1）：
 * - T1 本地 ConPTY 会话（PowerShell / 自定义命令行）+ T5 WSL 分发
 * - T2 输出经 term.output 事件写入 xterm；ack 背压（累计字节回传）
 * - T3 SSH 会话（密码/密钥，TOFU 指纹自动记录，变更报错）+ SFTP 列表/上传/下载
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
  // SFTP
  const [sftpPath, setSftpPath] = useState("/root");
  const [sftpEntries, setSftpEntries] = useState<SftpEntryDto[]>([]);
  const [sftpRemote, setSftpRemote] = useState("");
  const [sftpLocal, setSftpLocal] = useState("");
  const [sftpBusy, setSftpBusy] = useState(false);

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
    } catch {
      /* 模块未就绪静默 */
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
      term.onResize((size) => void termResize(s.id, size.cols, size.rows).catch(() => {}));
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
    void termWslList().then(setWsl).catch(() => {});
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
              void termAck(session_id, entry.received).catch(() => {});
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

  const spawnSsh = useCallback(async () => {
    if (!sshHost.trim()) return;
    setErr(null);
    try {
      const s = await termSshConnect({
        host: sshHost.trim(),
        port: Number(sshPort) || 22,
        user: sshUser,
        auth: sshAuth(),
        cols: 100,
        rows: 26,
      });
      setSessions((prev) => [...prev, s]);
      setActive(s.id);
      setMsg(`SSH 已连接 ${s.title}`);
    } catch (e) {
      fail(e);
    }
  }, [sshHost, sshPort, sshUser, sshAuth, fail]);

  const killSession = useCallback(
    async (id: string) => {
      try {
        await termKill(id);
        termsRef.current.delete(id);
        await refreshSessions();
      } catch (e) {
        fail(e);
      }
    },
    [refreshSessions, fail],
  );

  const sftpTarget = useCallback(() => {
    return { host: sshHost.trim(), port: Number(sshPort) || 22, user: sshUser, auth: sshAuth() };
  }, [sshHost, sshPort, sshUser, sshAuth]);

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

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <button className={`${styles.tab} ${tab === "term" ? styles.tabActive : ""}`} onClick={() => setTab("term")}>
          终端（{sessions.filter((s) => s.alive).length} 活跃）
        </button>
        <button className={`${styles.tab} ${tab === "docker" ? styles.tabActive : ""}`} onClick={() => setTab("docker")}>
          Docker
        </button>
      </div>

      {msg && <Text className={styles.ok}>{msg}</Text>}
      {err && <Text className={styles.error}>{err}</Text>}

      {tab === "term" && (
        <div className={styles.section}>
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
                    void killSession(s.id);
                  }}
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
          {sftpEntries.length > 0 && (
            <div className={styles.list}>
              {sftpEntries.map((e) => (
                <div
                  key={e.name}
                  className={`${styles.item} ${styles.itemHover}`}
                  onClick={() => {
                    const next = e.is_dir ? `${sftpPath.replace(/\/$/, "")}/${e.name}` : `${sftpPath.replace(/\/$/, "")}/${e.name}`;
                    setSftpPath(next);
                    if (e.is_dir) void loadSftp();
                  }}
                >
                  <Text size={200}>{e.is_dir ? "📁" : "📄"} {e.name}</Text>
                  {!e.is_dir && (
                    <Text size={100} className={styles.muted}>
                      {(e.size / 1024).toFixed(1)} KB
                    </Text>
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
        </div>
      )}

      {tab === "docker" && (
        <div className={styles.section}>
          <div className={styles.row}>
            <Text size={300} weight="semibold">
              容器（Docker Desktop 需运行中）
            </Text>
            <div className={styles.grow} />
            <Button size="small" onClick={() => void loadContainers()}>
              刷新
            </Button>
          </div>
          {dockerLoading && <Spinner size="tiny" />}
          {containers && containers.length === 0 && !dockerLoading && (
            <Text className={styles.muted}>无容器（或 Docker Engine 不可达）</Text>
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
                  <Button size="small" appearance="subtle" onClick={() => void lifecycle(c.id, false)}>
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
        </div>
      )}
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
