import { create } from "zustand";
import { reportError } from "./notifications";

/**
 * 到期提醒缓冲 store（09 §4.2 T-B1-6）：
 * `desktop.remind_due` 事件由后端 30s 轮询线程发布（desktop-core module.rs），
 * 旧实现只有 DesktopPanel 挂窗时监听——面板未打开即永久丢提醒。
 * 订阅上移到主窗口级 feed（MainWorkbench 挂载时呼起，同 D-14 startModuleStatusFeed 模式），
 * 缓冲封顶 20 条（新→旧）。到期消费是破坏性的（take_due 置 reminded=1），
 * 故刻意不接 `desktop_notes_due` 手动拉取命令，避免与后台轮询抢消费。
 */

/** desktop.remind_due 事件载荷（后端 json! 四字段，非完整 Note DTO） */
export interface RemindDueDto {
  id: string;
  content: string;
  remind_at: number | null;
  tags: string[];
}

const MAX_DUE = 20;

interface DesktopRemindersState {
  /** 未确认的到期提醒（新→旧） */
  due: RemindDueDto[];
  pushDue: (n: RemindDueDto) => void;
  /** 面板「知道了」：按 id 移除单条 */
  dismissDue: (id: string) => void;
  clearDue: () => void;
}

export const useDesktopReminders = create<DesktopRemindersState>((set) => ({
  due: [],
  pushDue: (n) => set((s) => ({ due: [n, ...s.due.filter((d) => d.id !== n.id)].slice(0, MAX_DUE) })),
  dismissDue: (id) => set((s) => ({ due: s.due.filter((d) => d.id !== id) })),
  clearDue: () => set({ due: [] }),
}));

let feedStarted = false;

/** nf:event → desktop.remind_due 订阅（幂等；主窗口挂载时调用一次） */
export function startDesktopRemindFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as { topic?: string; payload?: RemindDueDto };
        if (env.topic !== "desktop.remind_due") return;
        const n = env.payload;
        if (n && typeof n.id === "string") useDesktopReminders.getState().pushDue(n);
      }),
    )
    .catch((err) => reportError(err, { context: "到期提醒事件订阅失败", toast: false }));
}
