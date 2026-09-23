import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";

/**
 * TOFU 对话框（T-B6-10，panels/04 §2 授权红线）：后端两枚错误消息是唯一事实源——
 * FILE_REMOTE_001 的"指纹（逐字）："（首见）与 FILE_REMOTE_005 的
 * "记录（逐字）：…实收（逐字）：…"（键变更）在此逐字复述，永不归一化、永不截断改写。
 * Changed 臂红色且唯一出路是取消：不存在"仍然连接"这枚钮（ssh.rs tofu_guard 文案
 * 同谱——"不存在带着旧记录继续连的出路"，UI 不得给出后端拒绝的承诺）。
 */

export type TofuRequest =
  | { arm: "unknown"; host: string; port: number; fingerprint: string }
  | { arm: "changed"; host: string; port: number; recorded: string; actual: string };

const UNKNOWN_RE = /^主机 (.+?):(\d+) 首次连接[\s\S]*指纹（逐字）：(.+?)。/;
const CHANGED_RE =
  /^主机 (.+?):(\d+) 密钥已变更[\s\S]*记录（逐字）：(.+?)。实收（逐字）：(.+?)。/;

/** 从典形错误的 code+message 解析 TOFU 请求；非 TOFU 形态（同为 001 的"档案不存在"等）返回 null */
export function parseTofuRequest(
  code: string | undefined,
  message: string,
): TofuRequest | null {
  if (code === "FILE_REMOTE_001") {
    const m = UNKNOWN_RE.exec(message);
    if (m) return { arm: "unknown", host: m[1], port: Number(m[2]), fingerprint: m[3] };
  }
  if (code === "FILE_REMOTE_005") {
    const m = CHANGED_RE.exec(message);
    if (m)
      return {
        arm: "changed",
        host: m[1],
        port: Number(m[2]),
        recorded: m[3],
        actual: m[4],
      };
  }
  return null;
}

const useStyles = makeStyles({
  body: { display: "flex", flexDirection: "column", gap: "10px" },
  mono: {
    fontFamily: "Consolas, Cascadia Mono, monospace",
    wordBreak: "break-all",
  },
  hint: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground3 },
  danger: { color: tokens.colorPaletteRedForeground1, fontWeight: tokens.fontWeightSemibold },
});

export interface TofuPromptDialogProps {
  request: TofuRequest | null;
  open: boolean;
  onCancel: () => void;
  /** 首见臂唯一确认口：回传**逐字**指纹，宿主负责 fileRemoteFingerprintAck + 重连 */
  onAck: (fingerprint: string) => void;
}

export default function TofuPromptDialog({
  request,
  open,
  onCancel,
  onAck,
}: TofuPromptDialogProps) {
  const styles = useStyles();
  if (!request) return null;
  const target = `${request.host}:${request.port}`;
  return (
    <Dialog open={open} onOpenChange={(_, d) => !d.open && onCancel()}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>
            {request.arm === "unknown" ? "确认服务器密钥（首次连接）" : "服务器密钥已变更"}
          </DialogTitle>
          <DialogContent>
            <div className={styles.body}>
              <Text>{target}</Text>
              {request.arm === "unknown" ? (
                <>
                  <Text className={styles.mono}>{request.fingerprint}</Text>
                  <Text className={styles.hint}>
                    请经带外渠道与服务器侧核对这枚指纹完全一致后再接受；
                    本客户端没有隐式自纳这回事。
                  </Text>
                </>
              ) : (
                <>
                  <Badge appearance="filled" color="danger">
                    连接已拒
                  </Badge>
                  <Text className={styles.danger}>记录（逐字）：</Text>
                  <Text className={styles.mono}>{request.recorded}</Text>
                  <Text className={styles.danger}>实收（逐字）：</Text>
                  <Text className={styles.mono}>{request.actual}</Text>
                  <Text className={styles.hint}>
                    若确认服务器已重建，先删除该主机的记录再发起连接；
                    这里不存在带着旧记录继续连接的出路。
                  </Text>
                </>
              )}
            </div>
          </DialogContent>
          <DialogActions>
            <Button onClick={onCancel}>取消</Button>
            {request.arm === "unknown" && (
              <Button
                appearance="primary"
                onClick={() => onAck(request.fingerprint)}
              >
                接受并连接
              </Button>
            )}
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
