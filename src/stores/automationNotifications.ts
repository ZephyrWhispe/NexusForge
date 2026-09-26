import { notify, reportError } from "./notifications";

/**
 * D-39①：自动化规则的 `Action::Notify` 落点。engine.rs/module.rs 把通知发到
 * `automation.notify` 事件总线，但此前前端零消费——规则里配"通知"动作等于静默
 * 无操作。挂主窗口级 feed（kvmPairAlerts 同谱）：RulesPanel 未打开也必须弹。
 */

interface NotifyEventEnv {
  topic?: string;
  payload?: { rule_id?: string; title?: string; body?: string };
}

let feedStarted = false;

/** nf:event → automation.notify 全局提醒订阅（幂等；主窗口挂载时调用一次） */
export function startAutomationNotifyFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as NotifyEventEnv;
        if (env.topic !== "automation.notify") return;
        const title = env.payload?.title;
        if (!title) return; // 无标题的通知无法表意，宁可不弹也不弹"undefined"
        notify("info", title, env.payload?.body ?? "");
      }),
    )
    .catch((err) => reportError(err, { context: "自动化通知事件订阅失败", toast: false }));
}
