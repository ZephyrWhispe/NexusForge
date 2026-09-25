/**
 * T-B8-1（D-33，09 §7.3-(b) 已交付）拖拽传文件纯核：裁决面与聚合面零 IO——
 * 接线层（窗口事件）与对话框层都吃这两个函数，jsdom 下全可测；
 * 真 drop 可达性属 WebView2 平台事实，登记实启冒烟（D-33 验收①）。
 */

export interface KvmClientDevice {
  deviceId: string;
  deviceName: string;
}

export type DropPlan =
  | { kind: "refuse"; reason: string }
  | { kind: "ready"; files: string[]; devices: KvmClientDevice[] };

export interface SendOutcome {
  path: string;
  error?: string;
}

/** drop 裁决：空集拒；无出站（client 角色）会话拒并点名前置动作——不开对话框；
 * 目录/非法路径不猜（前端无 fs 权），交后端 kvm_send_file 逐字错误面点名。 */
export function planDropSend(
  paths: readonly string[],
  devices: readonly KvmClientDevice[],
): DropPlan {
  const files = paths.filter((p) => p.trim() !== "");
  if (files.length === 0) return { kind: "refuse", reason: "未拖入任何文件" };
  if (devices.length === 0)
    return {
      kind: "refuse",
      reason: "拖拽发送需要本端发起的出站会话：请先在设备列表点「连接」（对端接入的会话不视为推送目标）",
    };
  return { kind: "ready", files, devices: [...devices] };
}

/** 逐条结果聚合：「成功 X · 失败 Y」+ 逐条失败行（含路径与错误原文）。 */
export function aggregateSends(outcomes: readonly SendOutcome[]): {
  ok: number;
  failed: number;
  lines: string[];
} {
  const lines = outcomes
    .filter((o) => o.error)
    .map((o) => `失败 ${o.path}：${o.error}`);
  return { ok: outcomes.length - lines.length, failed: lines.length, lines };
}
