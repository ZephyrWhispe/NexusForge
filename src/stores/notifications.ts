import { create } from "zustand";
import { hostLog, parseAppError } from "../ipc/client";

/**
 * 全局错误/通知通道（docs/DESIGN.md §8.2，审查 D-19）：
 * 任何 IPC/异步失败都不允许静默吞掉——至少进宿主日志 + StatusBar 错误角标，
 * 默认再弹一条 toast。toast:false 供高频低价值路径（ack、轮询）做静默聚合上报。
 */

export type NoteKind = "error" | "warn" | "info" | "success";

export interface Notification {
  id: number;
  kind: NoteKind;
  title: string;
  body?: string;
  ts: number;
}

interface NotificationsState {
  /** 通知历史（新→旧，封顶 30 条），供 StatusBar 角标点击回放 */
  notes: Notification[];
  /** 当前正在展示的 toast id（先进先出，最多 4 条同屏） */
  visible: number[];
  /** 未读错误数（StatusBar 红点角标） */
  unseenErrors: number;
  push: (kind: NoteKind, title: string, body?: string, toast?: boolean) => number;
  dismiss: (id: number) => void;
  /** 点击角标：把最近的错误重新弹出来并清零角标 */
  flushUnseenErrors: () => void;
}

let nextId = 1;
const MAX_NOTES = 30;
const MAX_VISIBLE = 4;
const LIFETIME_MS: Record<NoteKind, number> = {
  error: 6500,
  warn: 5000,
  info: 3200,
  success: 3200,
};

export const useNotifications = create<NotificationsState>((set, get) => ({
  notes: [],
  visible: [],
  unseenErrors: 0,

  push(kind, title, body, toast = true) {
    const note: Notification = { id: nextId++, kind, title, body, ts: Date.now() };
    set((s) => ({
      notes: [note, ...s.notes].slice(0, MAX_NOTES),
      unseenErrors: kind === "error" ? s.unseenErrors + 1 : s.unseenErrors,
    }));
    if (toast) {
      set((s) => {
        const visible = [...s.visible, note.id].slice(-MAX_VISIBLE);
        const dropped = s.visible.filter((id) => !visible.includes(id));
        dropped.forEach((id) => clearTimeout(lifeOf(id)));
        return { visible };
      });
      const timer = window.setTimeout(() => get().dismiss(note.id), LIFETIME_MS[kind]);
      timers.set(note.id, timer);
    }
    return note.id;
  },

  dismiss(id) {
    clearTimeout(timers.get(id));
    timers.delete(id);
    set((s) => ({ visible: s.visible.filter((v) => v !== id) }));
  },

  flushUnseenErrors() {
    const recent = get().notes.filter((n) => n.kind === "error").slice(0, MAX_VISIBLE);
    recent.forEach((n) => get().push("error", n.title, n.body));
    // 回放自身会重新计数，必须放在 push 之后清零
    set({ unseenErrors: 0 });
  },
}));

/** push 的自动消失定时器（dismiss 时清除） */
const timers = new Map<number, number>();
function lifeOf(id: number): number {
  return timers.get(id) ?? 0;
}

/** 便捷入口（成功/信息提示走全局 toast，替代面板内自制 showToast） */
export function notify(kind: NoteKind, title: string, body?: string): number {
  return useNotifications.getState().push(kind, title, body);
}

export interface ReportErrorOptions {
  /** 中文语境标题，如 "目录读取失败"；缺省 "操作失败" */
  context?: string;
  /** 去重键：窗口期内同键只弹一次 toast（日志仍每条都进）。高频轮询/每帧路径必传 */
  dedupeKey?: string;
  /** false = 只进日志与角标，不弹 toast */
  toast?: boolean;
}

const DEDUPE_WINDOW_MS = 3000;
const lastReport = new Map<string, number>();

/** 非 AppError（DOMException、Tauri 窗口事件对象等）的可读化 */
function humanize(e: unknown): string {
  const s = String(e);
  if (s !== "[object Object]") return s;
  try {
    return JSON.stringify(e);
  } catch {
    return s;
  }
}

/**
 * monaco-editor 销毁编辑器时会把它内部各 Delayer 的在途 Promise 逐个以
 * Canceled 错误 reject（上游行为，业务侧无法根治）。这类 rejection 进全局
 * 通道会把宿主日志刷屏（实启冒烟实测：切离编辑器面板 3 秒内 ~200 条
 * 「未处理的异步错误: Canceled」），按栈特征精确滤除；无 monaco 栈的
 * Canceled 一律照常上报，不掩护真实错误。
 */
export function isMonacoTeardownCanceled(reason: unknown): boolean {
  const r = reason as { name?: unknown; stack?: unknown } | null | undefined;
  return (
    r?.name === "Canceled" &&
    typeof r?.stack === "string" &&
    r.stack.includes("Delayer.cancel")
  );
}

/**
 * 全局错误上报：normalize AppErrorDto → 宿主日志（必有）→ 错误角标（必有）→ toast（默认）。
 * 幂等防递归：hostLog 自身失败不再回流 reportError。
 */
export function reportError(e: unknown, opts: ReportErrorOptions = {}): void {
  const err = parseAppError(e);
  const code = err?.data.code;
  const message = err?.data.message ?? humanize(e);
  const hint = err?.data.hint ? `（${err.data.hint}）` : "";
  const detail = `${code ? `[${code}] ` : ""}${message}${hint}`;
  const title = opts.context ?? "操作失败";

  hostLog("error", `${title}: ${detail}`);

  if (opts.dedupeKey) {
    const now = Date.now();
    const last = lastReport.get(opts.dedupeKey);
    if (last !== undefined && now - last < DEDUPE_WINDOW_MS) return;
    lastReport.set(opts.dedupeKey, now);
  }
  useNotifications.getState().push("error", title, detail, opts.toast !== false);
}
