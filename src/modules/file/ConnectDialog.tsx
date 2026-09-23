import { useState } from "react";
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Input,
  Label,
  makeStyles,
  Radio,
  RadioGroup,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  fileRemoteConnect,
  parseAppError,
  type AuthKindDto,
  type RemoteDriverDto,
  type RemoteProfileDto,
} from "../../ipc/client";
import { confirmAction } from "../../stores/confirm";
import { notify, reportError } from "../../stores/notifications";

/**
 * B6 T-B6-8 连接对话框（形状先行）：authKind 五档切换 = **整表单重挂载**
 * （keyed section，见 connectionDialog_authKindSwitch_rerendersWholeForm）——
 * 档位间不共享任何输入节点，口令值不可能跨档残留。挂载与档案编辑归 T-B6-10
 * （ConnectionsSection 三态钮），本组件先立"换档=换表单"的裁决面。
 *
 * 凭据生命周期：口令只活在本组件 state，提交后立刻清空（成功/失败都清）；
 * 组件不写档案、不进 session store——落盘面永不出凭据。
 */

const useStyles = makeStyles({
  body: { display: "flex", flexDirection: "column", gap: "10px" },
  hint: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground3 },
  err: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorPaletteRedForeground1,
    wordBreak: "break-all",
  },
  field: { display: "flex", flexDirection: "column", gap: "4px" },
});

type AuthKindTag = AuthKindDto["kind"];

const AUTH_LABELS: Record<AuthKindTag, string> = {
  anonymous: "匿名",
  ssh_key: "SSH 私钥（指针）",
  vault_entry: "保险库条目（指针）",
  prompt_each_time: "每次输入",
  session_password: "本会话口令",
};

export interface ConnectDialogProps {
  profile: RemoteProfileDto;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 连接成功回调（宿主刷新远端列表用）；组件自身不留连接态 */
  onConnected?: (info: RemoteDriverDto) => void;
}

export default function ConnectDialog({
  profile,
  open,
  onOpenChange,
  onConnected,
}: ConnectDialogProps) {
  const styles = useStyles();
  const [authKind, setAuthKind] = useState<AuthKindTag>(profile.auth.kind);
  const [keyPath, setKeyPath] = useState(
    profile.auth.kind === "ssh_key" ? profile.auth.key_path : "",
  );
  const [entryId, setEntryId] = useState(
    profile.auth.kind === "vault_entry" ? profile.auth.entry_id : "",
  );
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const switchKind = (next: AuthKindTag) => {
    // 换档即丢值：指针与口令都只在各自档位有意义，跨档携带是脏不是便利
    setAuthKind(next);
    setPassword("");
    setKeyPath("");
    setEntryId("");
    setErr(null);
  };

  const typedValue = authKind === "prompt_each_time" || authKind === "session_password";

  const doConnect = async (allowPlaintextOnce: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      // 只有逐次输入档才组凭据体；指针档由宿主层经真 vault 解析（前端不碰值）
      const secret = typedValue && password.length > 0 ? { password } : null;
      const info = await fileRemoteConnect(profile.id, secret, allowPlaintextOnce);
      notify(
        "success",
        `已连接 ${info.label}`,
        `凭据来源：${info.auth_source}（只报来源，不报值）。`,
      );
      onConnected?.(info);
      onOpenChange(false);
    } catch (e) {
      const ae = parseAppError(e);
      if (ae?.data.code === "FILE_REMOTE_008" && !allowPlaintextOnce) {
        // 明文第三闸：逐次明示——复述目标地址，确认只活过这一次调用
        const ok = await confirmAction({
          title: "确认明文连接？",
          impact: [
            `目标：${profile.host}:${profile.port}（协议 ${profile.protocol}，明文过网）`,
            "口令与文件内容将以可读明文经过网络路径",
          ],
          detail: "本确认只对本次连接生效，不留记忆位；档案里永不存在口令字段。",
          confirmLabel: "本次以明文连接",
          danger: true,
        });
        if (ok) {
          await doConnect(true);
          return;
        }
        setErr("已取消：明文连接未获用户明示（FILE_REMOTE_008）");
      } else if (ae) {
        setErr(`${ae.data.code}: ${ae.data.message}`);
      } else {
        reportError(e, { context: "远端连接异常" });
        setErr("连接失败（非典形错误，已上报宿主日志）");
      }
    } finally {
      setPassword("");
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={(_, d) => !d.open && onOpenChange(false)}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>连接 {profile.label}</DialogTitle>
          <DialogContent>
            <div className={styles.body}>
              <Text>
                {profile.user}@{profile.host}:{profile.port}
              </Text>
              <RadioGroup
                name={`auth-${profile.id}`}
                value={authKind}
                onChange={(_, data) => switchKind(data.value as AuthKindTag)}
              >
                {(Object.keys(AUTH_LABELS) as AuthKindTag[]).map((k) => (
                  <Radio key={k} value={k} label={AUTH_LABELS[k]} />
                ))}
              </RadioGroup>
              {/* keyed section：档位一变，下方全部输入节点重挂载（DOM 级证据） */}
              <div key={authKind} data-testid={`auth-form-${authKind}`}>
                {authKind === "anonymous" && (
                  <Text className={styles.hint}>匿名档不索取任何凭据（typed 入参会被就地退役）。</Text>
                )}
                {authKind === "ssh_key" && (
                  <div className={styles.field}>
                    <Label htmlFor="cd-keypath">私钥路径（指针，非密钥内容）</Label>
                    <Input
                      id="cd-keypath"
                      value={keyPath}
                      onChange={(_, d) => setKeyPath(d.value)}
                      placeholder="C:\\Users\\me\\.ssh\\id_ed25519"
                    />
                  </div>
                )}
                {authKind === "vault_entry" && (
                  <div className={styles.field}>
                    <Label htmlFor="cd-entryid">保险库条目 id（指针，非口令）</Label>
                    <Input
                      id="cd-entryid"
                      value={entryId}
                      onChange={(_, d) => setEntryId(d.value)}
                      placeholder="0198f2c7-…"
                    />
                    <Text className={styles.hint}>
                      值由宿主层经解锁的保险库解析；未解锁/缺条目各态独立报错，不回落"请输入口令"。
                    </Text>
                  </div>
                )}
                {typedValue && (
                  <div className={styles.field}>
                    <Label htmlFor="cd-pass">口令（本次有效，提交后立即清空）</Label>
                    <Input
                      id="cd-pass"
                      type="password"
                      value={password}
                      onChange={(_, d) => setPassword(d.value)}
                    />
                  </div>
                )}
              </div>
              {profile.protocol === "ftp" && (
                <Badge appearance="outline" color="severe">
                  明文协议：非回环地址将逐次要求明示确认
                </Badge>
              )}
              {err && (
                <Text className={styles.err} role="alert">
                  {err}
                </Text>
              )}
            </div>
          </DialogContent>
          <DialogActions>
            <Button onClick={() => onOpenChange(false)}>取消</Button>
            <Button
              appearance="primary"
              disabled={busy}
              onClick={() => void doConnect(false)}
            >
              {busy ? "连接中…" : "连接"}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
