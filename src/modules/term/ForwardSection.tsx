import { useCallback, useEffect, useMemo, useState } from "react";
import { Badge, Button, Checkbox, Dropdown, Input, Option, Text, tokens } from "@fluentui/react-components";
import {
  termForwardClose,
  termForwardList,
  termForwardOpen,
  type ForwardKindDto,
  type ForwardSpecDto,
  type TermSessionDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";

/**
 * 端口转发管理表（T-B7-5，红线批：端口暴露）。
 * - 转发只挂在**活着的 SSH 会话**上（会话关即全拆），故选样只列 alive+ssh
 * - 每行真 state 徽标：listening / refused / closed——**端口占用显示被拒行而非空表**
 *   （后端 open 撞端口即回 Refused 并进表，静默换端口=你以为转对了）
 * - 非回环绑定是明示决定：须地址 + 确认位齐备才发得出去（bind_gate 的 UI 镜像）
 */
type KindSel = "local" | "remote" | "dynamic";

function stateOf(s: ForwardSpecDto["state"]): { key: string; label: string; color: "success" | "danger" | "subtle" } {
  if (typeof s === "string") return { key: "closed", label: "已关闭", color: "subtle" };
  if ("listening" in s)
    return { key: "listening", label: `监听 :${s.listening.bound_port}`, color: "success" };
  return { key: "refused", label: "被拒", color: "danger" };
}

function reasonOf(s: ForwardSpecDto["state"]): string | null {
  if (typeof s === "object" && "refused" in s) return s.refused.reason;
  return null;
}

function kindLabel(k: ForwardKindDto): string {
  if ("local" in k) return `-L ${k.local.listen_port} → ${k.local.dest_host}:${k.local.dest_port}`;
  if ("remote" in k)
    return `-R ${k.remote.bind === "127.0.0.1" ? "127.0.0.1" : `(${JSON.stringify(k.remote.bind)})`}:${k.remote.listen_port} → ${k.remote.dest_host}:${k.remote.dest_port}`;
  return `-D SOCKS5 :${k.dynamic.listen_port}`;
}

export default function ForwardSection({
  sessions,
  activeId,
}: {
  sessions: TermSessionDto[];
  activeId: string | null;
}) {
  const sshSessions = useMemo(
    () => sessions.filter((s) => s.alive && s.kind.kind === "ssh"),
    [sessions],
  );
  const [selected, setSelected] = useState<string | null>(null);
  const sessionId = selected
    ? sshSessions.some((s) => s.id === selected)
      ? selected
      : (sshSessions[0]?.id ?? null)
    : (sshSessions.find((s) => s.id === activeId)?.id ?? sshSessions[0]?.id ?? null);

  const [rows, setRows] = useState<ForwardSpecDto[] | null>(null);
  const [kind, setKind] = useState<KindSel>("local");
  const [listenPort, setListenPort] = useState("");
  const [destHost, setDestHost] = useState("");
  const [destPort, setDestPort] = useState("");
  const [nonLoopback, setNonLoopback] = useState(false);
  const [bindAddr, setBindAddr] = useState("");
  const [acknowledged, setAcknowledged] = useState(false);

  const refresh = useCallback(async (id: string) => {
    try {
      setRows(await termForwardList(id));
    } catch (e) {
      reportError(e, { context: "转发列表读取失败", toast: false });
      setRows([]);
    }
  }, []);

  useEffect(() => {
    if (sessionId) void refresh(sessionId);
    else setRows(null);
  }, [sessionId, refresh]);

  const canOpen =
    !!sessionId &&
    Number(listenPort) > 0 &&
    (kind === "dynamic" || (destHost.trim() !== "" && Number(destPort) > 0)) &&
    (kind !== "remote" || !nonLoopback || (acknowledged && bindAddr.trim() !== ""));

  const buildSpec = (): ForwardKindDto | null => {
    const lp = Number(listenPort);
    if (!lp || lp < 1 || lp > 65535) return null;
    if (kind === "dynamic") return { dynamic: { listen_port: lp } };
    const dh = destHost.trim();
    const dp = Number(destPort);
    if (!dh || !dp || dp < 1 || dp > 65535) return null;
    if (kind === "local") return { local: { listen_port: lp, dest_host: dh, dest_port: dp } };
    const bind = nonLoopback
      ? { other: { addr: bindAddr.trim(), acknowledged: true } }
      : ("127.0.0.1" as const);
    return { remote: { bind, listen_port: lp, dest_host: dh, dest_port: dp } };
  };

  const open = async () => {
    if (!sessionId) return;
    const spec = buildSpec();
    if (!spec) return;
    try {
      // 返回即终态：Listening/Refused 都进表——失败不静默，重取列表面板见被拒行
      await termForwardOpen(sessionId, spec);
      await refresh(sessionId);
      setListenPort("");
      setDestHost("");
      setDestPort("");
    } catch (e) {
      reportError(e, { context: "开启转发失败" });
    }
  };

  const close = async (forwardId: string) => {
    if (!sessionId) return;
    try {
      await termForwardClose(forwardId);
      await refresh(sessionId);
    } catch (e) {
      reportError(e, { context: "关闭转发失败" });
    }
  };

  if (sshSessions.length === 0) {
    return (
      <EmptyState text="无活动 SSH 会话——端口转发挂在 SSH 会话上，先连接一台主机" />
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        <Dropdown
          placeholder="选择 SSH 会话"
          value={sshSessions.find((s) => s.id === sessionId)?.title ?? ""}
          selectedOptions={sessionId ? [sessionId] : []}
          onOptionSelect={(_, d) => setSelected(String(d.optionValue ?? ""))}
        >
          {sshSessions.map((s) => (
            <Option key={s.id} value={s.id} text={s.title}>
              {s.title}
            </Option>
          ))}
        </Dropdown>
        <Dropdown
          placeholder="类型"
          value={kind === "local" ? "-L 本地" : kind === "remote" ? "-R 远端" : "-D SOCKS5"}
          selectedOptions={[kind]}
          onOptionSelect={(_, d) => setKind(String(d.optionValue ?? "local") as KindSel)}
        >
          <Option value="local" text="-L 本地">-L 本地</Option>
          <Option value="remote" text="-R 远端">-R 远端</Option>
          <Option value="dynamic" text="-D SOCKS5">-D SOCKS5</Option>
        </Dropdown>
        <Input style={{ maxWidth: 110 }} placeholder="监听端口" value={listenPort} onChange={(_, d) => setListenPort(d.value)} />
        {kind !== "dynamic" ? (
          <>
            <Input style={{ maxWidth: 160 }} placeholder="目标主机" value={destHost} onChange={(_, d) => setDestHost(d.value)} />
            <Input style={{ maxWidth: 110 }} placeholder="目标端口" value={destPort} onChange={(_, d) => setDestPort(d.value)} />
          </>
        ) : null}
        <Button appearance="primary" size="small" disabled={!canOpen} onClick={() => void open()}>
          新增转发
        </Button>
      </div>

      {kind === "remote" ? (
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          {/* D-43 C5：浏览器原生 checkbox 换 Fluent Checkbox（规范 0 节），
              确认位仍是 bind_gate 的 UI 镜像——勾选才解禁「新增转发」 */}
          <Checkbox
            checked={nonLoopback}
            onChange={(_, d) => setNonLoopback(d.checked === true)}
            label="非回环绑定（默认仅 127.0.0.1）"
          />
          {nonLoopback ? (
            <>
              <Input style={{ maxWidth: 160 }} placeholder="绑定地址（IP 字面量）" value={bindAddr} onChange={(_, d) => setBindAddr(d.value)} />
              <Checkbox
                checked={acknowledged}
                onChange={(_, d) => setAcknowledged(d.checked === true)}
                label="我确认把该端口暴露到非回环地址"
              />
            </>
          ) : null}
        </div>
      ) : null}

      {rows === null ? (
        <EmptyState text="转发列表加载中" loading />
      ) : rows.length === 0 ? (
        <EmptyState text="暂无转发" />
      ) : (
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {rows.map((r) => {
            const st = stateOf(r.state);
            const reason = reasonOf(r.state);
            return (
              <div
                key={r.id}
                data-fwd-row={r.id}
                style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}
              >
                <Badge appearance="filled" color={st.color} data-fwd-state={st.key}>
                  {st.label}
                </Badge>
                <Text size={200} weight="semibold">
                  {kindLabel(r.kind)}
                </Text>
                {reason ? (
                  <Text size={200} style={{ color: tokens.colorPaletteRedForeground1 }}>
                    {reason}
                  </Text>
                ) : null}
                <div style={{ flex: 1 }} />
                {st.key === "listening" ? (
                  <Button size="small" appearance="subtle" onClick={() => void close(r.id)}>
                    关闭
                  </Button>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
