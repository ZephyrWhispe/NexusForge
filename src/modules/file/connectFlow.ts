import {
  fileRemoteConnect,
  parseAppError,
  type AuthSecretDto,
  type RemoteDriverDto,
  type RemoteProfileDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import { parseTofuRequest, type TofuRequest } from "./TofuPromptDialog";

/**
 * 连接唯一管线（T-B6-10，09 §6.2）：明文逐次闸（008）与 TOFU（001/005）都在这
 * 一处裁决，ConnectDialog 与 ConnectionsSection 共用——面板自造第二台连接引擎
 * 是被禁的那一族复发（B5"决议≌换策略重入队"同谱）。
 */

export type ConnectOutcome =
  | { status: "connected"; info: RemoteDriverDto; rttMs: number }
  | { status: "cancelled_plaintext" }
  | { status: "tofu"; request: TofuRequest }
  | { status: "error"; display: string };

export type ConnectButtonState = "idle" | "connecting" | "connected";

/** 三态按钮唯一决策点：面板禁自拼"转圈位+勾位"两个布尔——connecting 优先于
 *  connected，两布尔并存这一不可能态被折叠成单一 connecting，转圈与绿勾不同屏。 */
export function connectButtonState(s: {
  connecting: boolean;
  connected: boolean;
}): ConnectButtonState {
  if (s.connecting) return "connecting";
  if (s.connected) return "connected";
  return "idle";
}

function failure(e: unknown): ConnectOutcome {
  const ae = parseAppError(e);
  if (ae) return { status: "error", display: `${ae.data.code}: ${ae.data.message}` };
  reportError(e, { context: "远端连接异常" });
  return { status: "error", display: "连接失败（非典形错误，已上报宿主日志）" };
}

async function attempt(
  profile: RemoteProfileDto,
  secret: AuthSecretDto | null | undefined,
  allowPlaintextOnce: boolean,
): Promise<ConnectOutcome> {
  const t0 = Date.now();
  try {
    const info = await fileRemoteConnect(profile.id, secret, allowPlaintextOnce);
    return { status: "connected", info, rttMs: Date.now() - t0 };
  } catch (e) {
    const ae = parseAppError(e);
    const code = ae?.data.code;
    const msg = ae?.data.message ?? "";
    if (code === "FILE_REMOTE_008" && !allowPlaintextOnce) {
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
      if (!ok) return { status: "cancelled_plaintext" };
      return attempt(profile, secret, true);
    }
    if (code === "FILE_REMOTE_001" || code === "FILE_REMOTE_005") {
      const tofu = parseTofuRequest(code, msg);
      if (tofu) return { status: "tofu", request: tofu };
    }
    return failure(e);
  }
}

/** 发起连接：secret 只进不出（AuthSecretDto 无 Serialize 形状），失败经典形错误分臂 */
export function runConnect(
  profile: RemoteProfileDto,
  secret?: AuthSecretDto | null,
): Promise<ConnectOutcome> {
  return attempt(profile, secret, false);
}
