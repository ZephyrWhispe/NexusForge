import { notify, reportError } from "./notifications";

/**
 * D-42：`kvm.clip_received` 主窗口级提醒。后端在剪贴板模块消费本主题时
 * 已经把对端内容写入本机剪贴板并按 origin=remote 入库（clipboard-core
 * module.rs:190 起，D-25），面板侧此前无人出声——用户只见剪切板内容"自己变了"。
 * 本 feed 只报"谁推来了剪贴板"这一事实，**不渲载荷内容**（隐私：内容可能含
 * 密码/令牌，而它本就已在本机剪切板与历史里，提醒不需要复述它）。
 */

interface ClipEventEnv {
  topic?: string;
  payload?: { device_id?: string };
}

let feedStarted = false;

/** nf:event → kvm.clip_received 提醒订阅（幂等；主窗口挂载时调用一次） */
export function startKvmClipAlertFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as ClipEventEnv;
        if (env.topic !== "kvm.clip_received") return;
        const who = env.payload?.device_id || "未知设备";
        notify(
          "info",
          `收到「${who}」共享的剪贴板`,
          "内容已写入本机剪贴板并作为远端来源进入历史（此处不显示内容）。",
        );
      }),
    )
    .catch((err) => reportError(err, { context: "KVM 剪贴板提醒事件订阅失败", toast: false }));
}
