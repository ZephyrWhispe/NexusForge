import { create } from "zustand";

/**
 * 破坏性操作二次确认通道（审查 D-18）：所有删除/覆盖/不可逆操作经
 * `confirmAction()` 弹全局 ConfirmDialog，展示影响面（条目数/文件数），
 * 可选 `command` 精确命令预览（SysPanel「命令预览确认」样板的推广）。
 * 队列 FIFO：并发请求逐个呈现，Esc/遮罩点击 = 取消当前项。
 */

export interface ConfirmOptions {
  title: string;
  /** 影响面说明行（如 "将删除 12 条记录"），字符串或逐行数组 */
  impact?: string | string[];
  /** 操作后果补充说明（灰色小字） */
  detail?: string;
  /** 将执行的确切命令/操作预览（等宽展示） */
  command?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** true（默认）= 危险红色确认钮；false = 普通主按钮 */
  danger?: boolean;
}

export interface ConfirmRequest extends ConfirmOptions {
  id: number;
}

/** impact 归一化为逐行数组（纯函数，供宿主与测试共用） */
export function impactLines(opts: Pick<ConfirmOptions, "impact">): string[] {
  if (opts.impact === undefined) return [];
  return Array.isArray(opts.impact) ? opts.impact : [opts.impact];
}

interface ConfirmState {
  queue: ConfirmRequest[];
  ask: (opts: ConfirmOptions) => Promise<boolean>;
  settleHead: (ok: boolean) => void;
}

let nextReqId = 1;
// resolve 回调不进 store（zustand 状态应保持可序列化）；id → resolve 侧表
const resolvers = new Map<number, (ok: boolean) => void>();

export const useConfirmStore = create<ConfirmState>((set, get) => ({
  queue: [],
  ask(opts) {
    const id = nextReqId++;
    set((s) => ({ queue: [...s.queue, { ...opts, id }] }));
    return new Promise<boolean>((resolve) => {
      resolvers.set(id, resolve);
    });
  },
  settleHead(ok) {
    const head = get().queue[0];
    if (!head) return;
    set((s) => ({ queue: s.queue.slice(1) }));
    const resolve = resolvers.get(head.id);
    resolvers.delete(head.id);
    resolve?.(ok);
  },
}));

/** 面板入口：`if (!(await confirmAction({...}))) return;` */
export function confirmAction(opts: ConfirmOptions): Promise<boolean> {
  return useConfirmStore.getState().ask(opts);
}
