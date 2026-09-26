import { notify, reportError } from "./notifications";

/**
 * D-39②：模块 panic 崩溃原因可见化。registry.rs 隔离模块失败时发
 * `host.module_crashed {module, message}`；`host.module_state` 只有状态迁移
 * （红点），崩溃的"为什么"仅存在于本主题——不接则用户只见模块变红不知缘由。
 */

interface CrashEventEnv {
  topic?: string;
  payload?: { module?: string; message?: string };
}

let feedStarted = false;

/** nf:event → host.module_crashed 强提醒订阅（幂等；主窗口挂载时调用一次） */
export function startModuleCrashAlertFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as CrashEventEnv;
        if (env.topic !== "host.module_crashed") return;
        const module = env.payload?.module ?? "未知模块";
        notify(
          "error",
          `模块崩溃已被宿主隔离：${module}`,
          `${env.payload?.message ?? "无错误信息"}（可在状态栏查看模块状态并尝试重启该模块）`,
        );
      }),
    )
    .catch((err) => reportError(err, { context: "模块崩溃提醒事件订阅失败", toast: false }));
}
