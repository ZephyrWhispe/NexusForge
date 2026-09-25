import { notify, reportError } from "./notifications";

/**
 * SEC-11（D-37 R-I1）配对成功强提醒：`kvm.paired` 事件（后端
 * kvm-core/module.rs 发布，paired=true 即"有设备刚把自己记为受信"）
 * 上移到主窗口级 feed 弹全局提醒——KvmPanel 未打开也必须可见，
 * 这是"配对可被局域网抢占"威胁画像下用户唯一能自主察觉的时刻。
 * 撤销入口不新造：提醒文案指向 KVM 面板既有的逐设备解绑（kvm_unpair）。
 */

interface PairedEventEnv {
  topic?: string;
  payload?: { peer?: { device_id?: string; device_name?: string }; paired?: boolean };
}

let feedStarted = false;

/** nf:event → kvm.paired 强提醒订阅（幂等；主窗口挂载时调用一次） */
export function startKvmPairAlertFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as PairedEventEnv;
        if (env.topic !== "kvm.paired" || env.payload?.paired !== true) return;
        const peer = env.payload.peer;
        const who = peer?.device_name || peer?.device_id || "未知设备";
        notify(
          "warn",
          "KVM 新配对完成",
          `设备「${who}」已通过一次性码配对为受信设备。若非本人操作，请立即在 KVM 面板解除其配对。`,
        );
      }),
    )
    .catch((err) => reportError(err, { context: "KVM 配对提醒事件订阅失败", toast: false }));
}
