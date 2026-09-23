import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  Input,
  Label,
  makeStyles,
  Radio,
  RadioGroup,
  Select,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  fileRemoteDisconnect,
  fileRemoteDrivers,
  fileRemoteFingerprintAck,
  fileRemoteProfileDelete,
  fileRemoteProfileSave,
  fileRemotePresets,
  fileRemoteProfiles,
  parseAppError,
  type AuthKindDto,
  type RemoteDriverDto,
  type RemoteProfileDto,
  type RemoteProtocolDto,
} from "../../ipc/client";
import { confirmAction } from "../../stores/confirm";
import { notify, reportError } from "../../stores/notifications";
import ConnectDialog from "./ConnectDialog";
import RemoteBrowser from "./RemoteBrowser";
import TofuPromptDialog, { type TofuRequest } from "./TofuPromptDialog";
import { connectButtonState, runConnect } from "./connectFlow";

/**
 * 连接与授权区（T-B6-10，panels/04 §2 FileZilla 站点管理器形制）：左站点列表
 * （协议图标 + 别名 + auth_source **只来源不值** + 最近使用），右档案表单
 * （换协议 = 整表单重挂载，旧字段值不残留——keyed section，同 ConnectDialog
 * 的换档纪律）。测试连接钮三态唯一决策点 = connectButtonState；结果就地
 * Badge（成功含 RTT——只报 runConnect 实测值，对话框路径无事实源即不写 RTT），
 * 失败红条可展开（错误文本已过 remote_error_message，永不含凭据）。
 *
 * 凭据红线：本组件零口令输入——逐次口令只活在 ConnectDialog；档案形状
 * (RemoteProfileDto) 结构性无凭据字段，保存走 fileRemoteProfileSave 原样转发。
 */

const useStyles = makeStyles({
  split: { flex: 1, minHeight: 0, display: "flex", gap: "12px" },
  list: {
    width: "280px",
    flexShrink: 0,
    minHeight: 0,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: "4px",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "8px",
  },
  siteRow: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    textAlign: "left",
    minWidth: 0,
  },
  siteRowActive: { backgroundColor: tokens.colorBrandBackground2 },
  form: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: "10px",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "10px 12px",
  },
  field: { display: "flex", flexDirection: "column", gap: "4px" },
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  err: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorPaletteRedForeground1,
    wordBreak: "break-all",
  },
  ellipsis: {
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
    minWidth: 0,
  },
});

const PROTOCOL_OPTIONS: { id: RemoteProtocolDto; label: string; icon: string }[] = [
  { id: "web_dav", label: "WebDAV", icon: "🌐" },
  { id: "https", label: "HTTPS 下载", icon: "⬇" },
  { id: "sftp", label: "SFTP", icon: "🔒" },
  { id: "ftp", label: "FTP（明文）", icon: "⚠" },
];

const AUTH_LABELS: Record<AuthKindDto["kind"], string> = {
  anonymous: "匿名",
  ssh_key: "密钥文件",
  vault_entry: "保险库条目",
  prompt_each_time: "每次询问",
  session_password: "本会话口令",
};

/** 左列表 auth_source 列：只报来源档位名，永不带值（单一算式，面板不自拼） */
export function authSourceOfKind(kind: AuthKindDto["kind"]): string {
  const map: Record<AuthKindDto["kind"], string> = {
    anonymous: "Anonymous",
    ssh_key: "KeyFile",
    vault_entry: "VaultEntry",
    prompt_each_time: "Typed",
    session_password: "Session",
  };
  return map[kind];
}

function isTypedKind(kind: AuthKindDto["kind"]): boolean {
  return kind === "prompt_each_time" || kind === "session_password";
}

function fmtTime(ms: number): string {
  if (!ms) return "";
  return new Date(ms).toLocaleString("zh-CN", { hour12: false });
}

function newDraft(): RemoteProfileDto {
  return {
    id: crypto.randomUUID(),
    label: "",
    protocol: "web_dav",
    host: "",
    port: 80,
    user: "",
    base_path: "/",
    auth: { kind: "anonymous" },
    preset_id: null,
    last_used_ms: 0,
  };
}

export default function ConnectionsSection() {
  const styles = useStyles();
  const [profiles, setProfiles] = useState<RemoteProfileDto[]>([]);
  const [drivers, setDrivers] = useState<RemoteDriverDto[]>([]);
  const [draft, setDraft] = useState<RemoteProfileDto>(newDraft());
  const [error, setError] = useState<string | null>(null);
  const [errExpanded, setErrExpanded] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [lastRttMs, setLastRttMs] = useState<number | null>(null);
  const [tofu, setTofu] = useState<TofuRequest | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [advanced, setAdvanced] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setProfiles(await fileRemoteProfiles());
    } catch (e) {
      const ae = parseAppError(e);
      if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
      else {
        reportError(e, { context: "连接档案列表异常" });
        setError("档案列表读取失败（非典形错误，已上报宿主日志）");
      }
    }
  }, []);

  // 已连接行经宿主广播刷新太吵；挂载拉一次 + 连接/断开动作点各自再拉即可
  const reloadDrivers = useCallback(async () => {
    try {
      setDrivers(await fileRemoteDrivers());
    } catch {
      setDrivers([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
    void reloadDrivers();
  }, [refresh, reloadDrivers]);

  const connected = drivers.some((d) => d.driver_id === draft.id);
  const btnState = connectButtonState({ connecting, connected });

  const showFailure = (text: string) => {
    setError(text);
    setErrExpanded(false);
    setLastRttMs(null);
  };

  const handleOutcome = async (r: Awaited<ReturnType<typeof runConnect>>) => {
    if (r.status === "connected") {
      setError(null);
      setLastRttMs(r.rttMs);
      await reloadDrivers();
    } else if (r.status === "cancelled_plaintext") {
      showFailure("已取消：明文连接未获用户明示（FILE_REMOTE_008）");
    } else if (r.status === "tofu") {
      setTofu(r.request);
    } else {
      showFailure(r.display);
    }
  };

  const doTest = async () => {
    setConnecting(true);
    setError(null);
    let saved: RemoteProfileDto;
    try {
      // 先落盘再连：连接以档案 id 为唯一事实源，草稿没有可连之物
      saved = await fileRemoteProfileSave(draft);
      setDraft(saved);
      await refresh();
    } catch (e) {
      setConnecting(false);
      const ae = parseAppError(e);
      showFailure(ae ? `${ae.data.code}: ${ae.data.message}` : "档案保存失败（非典形错误，已上报宿主日志）");
      if (!ae) reportError(e, { context: "连接档案保存异常" });
      return;
    }
    if (isTypedKind(saved.auth.kind)) {
      // 逐次口令档：凭据输入只在 ConnectDialog（本组件零口令字段，防第二输入面）
      setConnecting(false);
      setDialogOpen(true);
      return;
    }
    const r = await runConnect(saved, null);
    setConnecting(false);
    await handleOutcome(r);
  };

  const onTofuAck = async (fingerprint: string) => {
    setTofu(null);
    try {
      await fileRemoteFingerprintAck(draft.id, fingerprint);
    } catch (e) {
      const ae = parseAppError(e);
      showFailure(ae ? `${ae.data.code}: ${ae.data.message}` : "指纹确认失败（非典形错误，已上报宿主日志）");
      if (!ae) reportError(e, { context: "指纹确认异常" });
      return;
    }
    setConnecting(true);
    const r = await runConnect(draft, null);
    setConnecting(false);
    await handleOutcome(r);
  };

  const doDisconnect = async () => {
    const driver = drivers.find((d) => d.driver_id === draft.id);
    if (!driver) return;
    try {
      await fileRemoteDisconnect(driver.driver_id);
      await reloadDrivers();
      setLastRttMs(null);
    } catch (e) {
      const ae = parseAppError(e);
      showFailure(ae ? `${ae.data.code}: ${ae.data.message}` : "断开失败（非典形错误，已上报宿主日志）");
      if (!ae) reportError(e, { context: "远端断开异常" });
    }
  };

  const doDelete = async () => {
    const ok = await confirmAction({
      title: "删除连接档案？",
      impact: [`档案：${draft.label || draft.id}（仅档案条目；保险库与私钥文件本体不受影响）`],
      detail: "删除后可由同名档案重建；本操作不注销任何凭据。",
      confirmLabel: "删除档案",
      danger: true,
    });
    if (!ok) return;
    try {
      await fileRemoteProfileDelete(draft.id);
      setDraft(newDraft());
      await refresh();
      notify("success", "档案已删除", "仅删除档案条目。");
    } catch (e) {
      const ae = parseAppError(e);
      showFailure(ae ? `${ae.data.code}: ${ae.data.message}` : "删除失败（非典形错误，已上报宿主日志）");
      if (!ae) reportError(e, { context: "连接档案删除异常" });
    }
  };

  const switchProtocol = async (proto: RemoteProtocolDto) => {
    // 换协议 = 整表单重挂载 + 字段清零（旧字段值残留是脏不是便利）：
    // 端口/根路径取该协议预设缺省（file_remote_presets 为事实源）
    let port = 80;
    let basePath = "/";
    try {
      const hit = (await fileRemotePresets()).find((p) => p.protocol === proto);
      if (hit) {
        port = hit.port;
        basePath = hit.base_path || "/";
      }
    } catch {
      /* 预设不可用 ⇒ 保留协议缺省，不假称已按预设填值 */
    }
    setDraft((d) => ({
      ...newDraft(),
      id: d.id,
      protocol: proto,
      port,
      base_path: basePath,
    }));
  };

  const selectProfile = (p: RemoteProfileDto) => setDraft(p);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "12px", flex: 1, minHeight: 0 }}>
      <div className={styles.split}>
        <div className={styles.list} aria-label="站点列表">
          <div className={styles.row}>
            <Text weight="semibold" size={300}>
              站点
            </Text>
            <span style={{ flex: 1 }} />
            <Button size="small" onClick={() => setDraft(newDraft())}>
              新建
            </Button>
          </div>
          {profiles.length === 0 && (
            <Text className={styles.muted}>尚无档案：右侧填写后「保存档案」即入列</Text>
          )}
          {profiles.map((p) => {
            const icon = PROTOCOL_OPTIONS.find((o) => o.id === p.protocol);
            return (
              <Button
                key={p.id}
                size="small"
                appearance="subtle"
                className={`${styles.siteRow} ${p.id === draft.id ? styles.siteRowActive : ""}`}
                onClick={() => selectProfile(p)}
              >
                <span aria-hidden>{icon?.icon ?? "·"}</span>
                <span className={styles.ellipsis}>{p.label || p.host}</span>
                <Badge appearance="outline">{authSourceOfKind(p.auth.kind)}</Badge>
                <Text className={styles.muted}>{fmtTime(p.last_used_ms)}</Text>
              </Button>
            );
          })}
        </div>

        <div className={styles.form}>
          {/* keyed section：协议一变，整个表单（含协议选择器本身）DOM 级重挂载 */}
          <div key={draft.protocol} data-testid={`conn-form-${draft.protocol}`}>
            <div className={styles.field}>
              <Label htmlFor="cs-proto">协议</Label>
              <Select
                id="cs-proto"
                size="small"
                value={draft.protocol}
                onChange={(_, d) => void switchProtocol(d.value as RemoteProtocolDto)}
              >
                {PROTOCOL_OPTIONS.map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.icon} {o.label}
                  </option>
                ))}
              </Select>
            </div>
            <div className={styles.field}>
              <Label htmlFor="cs-label">别名</Label>
              <Input id="cs-label" size="small" value={draft.label} onChange={(_, d) => setDraft({ ...draft, label: d.value })} />
            </div>
            <div className={styles.row}>
              <div className={styles.field} style={{ flex: 1 }}>
                <Label htmlFor="cs-host">主机</Label>
                <Input id="cs-host" size="small" value={draft.host} onChange={(_, d) => setDraft({ ...draft, host: d.value })} />
              </div>
              <div className={styles.field} style={{ width: "100px" }}>
                <Label htmlFor="cs-port">端口</Label>
                <Input
                  id="cs-port"
                  size="small"
                  type="number"
                  value={String(draft.port)}
                  onChange={(_, d) => setDraft({ ...draft, port: Number(d.value) || 0 })}
                />
              </div>
            </div>
            <div className={styles.field}>
              <Label htmlFor="cs-user">用户</Label>
              <Input id="cs-user" size="small" value={draft.user} onChange={(_, d) => setDraft({ ...draft, user: d.value })} />
            </div>
            <div className={styles.field}>
              <Text size={200} className={styles.muted}>
                认证方式（本表单永不做口令输入；逐次口令只活在连接对话框）
              </Text>
              <RadioGroup
                name={`cs-auth-${draft.id}`}
                value={draft.auth.kind}
                onChange={(_, d) => {
                  const kind = d.value as AuthKindDto["kind"];
                  const auth: AuthKindDto =
                    kind === "ssh_key"
                      ? { kind, key_path: "" }
                      : kind === "vault_entry"
                        ? { kind, entry_id: "" }
                        : { kind } as AuthKindDto;
                  setDraft({ ...draft, auth });
                }}
              >
                {(Object.keys(AUTH_LABELS) as AuthKindDto["kind"][]).map((k) => (
                  <Radio key={k} value={k} label={AUTH_LABELS[k]} />
                ))}
              </RadioGroup>
              {draft.auth.kind === "ssh_key" && (
                <div className={styles.field}>
                  <Label htmlFor="cs-keypath">私钥路径（指针，非密钥内容）</Label>
                  <Input
                    id="cs-keypath"
                    size="small"
                    value={draft.auth.key_path}
                    onChange={(_, d) =>
                      setDraft({ ...draft, auth: { kind: "ssh_key", key_path: d.value } })
                    }
                  />
                </div>
              )}
              {draft.auth.kind === "vault_entry" && (
                <div className={styles.field}>
                  <Label htmlFor="cs-entryid">保险库条目 id（指针，非口令）</Label>
                  <Input
                    id="cs-entryid"
                    size="small"
                    value={draft.auth.entry_id}
                    onChange={(_, d) =>
                      setDraft({ ...draft, auth: { kind: "vault_entry", entry_id: d.value } })
                    }
                  />
                </div>
              )}
            </div>
            <Button size="small" appearance="subtle" onClick={() => setAdvanced(!advanced)}>
              {advanced ? "收起高级" : "高级"}
            </Button>
            {advanced && (
              <div className={styles.field}>
                <Label htmlFor="cs-base">远端根路径</Label>
                <Input
                  id="cs-base"
                  size="small"
                  value={draft.base_path}
                  onChange={(_, d) => setDraft({ ...draft, base_path: d.value })}
                />
              </div>
            )}
          </div>

          <div className={styles.row}>
            {error && (
              <div style={{ flex: 1, minWidth: 0 }}>
                <Badge appearance="filled" color="danger">
                  失败
                </Badge>
                <Text role="alert" className={styles.err}>
                  {errExpanded || error.length <= 80 ? error : `${error.slice(0, 80)}…`}
                </Text>{" "}
                <Button size="small" appearance="subtle" onClick={() => setErrExpanded(!errExpanded)}>
                  {errExpanded ? "收起" : "展开"}
                </Button>
              </div>
            )}
            {btnState === "connected" && !error && (
              <Badge appearance="filled" color="success">
                已连接
                {lastRttMs !== null ? ` · RTT ${lastRttMs}ms` : ""}
                {/* 无实测即无 RTT 文案：ConnectDialog 路径不假报对时 */}
              </Badge>
            )}
            <span style={{ flex: 1 }} />
            <Button size="small" onClick={() => void doDelete()}>
              删除档案
            </Button>
            <Button
              size="small"
              onClick={async () => {
                try {
                  const saved = await fileRemoteProfileSave(draft);
                  setDraft(saved);
                  await refresh();
                  notify("success", "档案已保存", "零凭据字段（形状里根本没有这种字段）。");
                } catch (e) {
                  const ae = parseAppError(e);
                  showFailure(ae ? `${ae.data.code}: ${ae.data.message}` : "保存失败（非典形错误，已上报宿主日志）");
                  if (!ae) reportError(e, { context: "连接档案保存异常" });
                }
              }}
            >
              保存档案
            </Button>
            {btnState === "connected" ? (
              <Button size="small" appearance="outline" onClick={() => void doDisconnect()}>
                断开
              </Button>
            ) : (
              <Button
                size="small"
                appearance="primary"
                disabled={btnState === "connecting" || !draft.host.trim()}
                onClick={() => void doTest()}
              >
                {btnState === "connecting" ? "连接中…" : "测试连接"}
              </Button>
            )}
          </div>
        </div>
      </div>

      <RemoteBrowser />

      {/* 每次打开都新挂载：ConnectDialog 的档位 state 以档案当前 auth 为初值，
          常驻挂载会让旧草稿的档位状态活过换档案（匿名草稿态串门进逐次档案） */}
      {dialogOpen && (
        <ConnectDialog
          profile={draft}
          open
          onOpenChange={(o) => {
            setDialogOpen(o);
            if (!o) void reloadDrivers();
          }}
          onConnected={() => {
            setLastRttMs(null);
            void reloadDrivers();
          }}
        />
      )}
      <TofuPromptDialog
        request={tofu}
        open={tofu !== null}
        onCancel={() => showFailure("已取消：主机密钥未获确认，连接未发起（TOFU）")}
        onAck={(fp) => void onTofuAck(fp)}
      />
    </div>
  );
}
