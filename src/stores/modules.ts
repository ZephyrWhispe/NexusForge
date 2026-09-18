import { create } from "zustand";
import { hostModulesStatus, type ModuleState } from "../ipc/client";
import { reportError } from "./notifications";

/**
 * 模块真实运行态 store（docs/DESIGN.md §7 Zustand 基线，审查 D-14）。
 * 初值取 host_modules_status 快照，此后完全由 host.module_state 事件驱动
 * （替代 StatusBar 原 2s 轮询）；导航绿点与状态栏共用同一事实源。
 */

interface ModulesState {
  /** 模块 id → 状态；未收录 = 宿主尚未上报（UI 回退展示阶段标签） */
  states: Record<string, ModuleState>;
  apply: (id: string, state: ModuleState) => void;
  replaceAll: (list: { id: string; state: ModuleState }[]) => void;
}

export const useModuleStatus = create<ModulesState>((set) => ({
  states: {},
  apply: (id, state) => set((s) => ({ states: { ...s.states, [id]: state } })),
  replaceAll: (list) => set({ states: Object.fromEntries(list.map((m) => [m.id, m.state])) }),
}));

let feedStarted = false;

/** 初始快照 + 事件订阅（幂等；主窗口挂载时调用一次） */
export function startModuleStatusFeed(): void {
  if (feedStarted) return;
  feedStarted = true;
  void hostModulesStatus().then((list) => {
    if (list) useModuleStatus.getState().replaceAll(list);
  });
  if (!("__TAURI_INTERNALS__" in window)) return;
  import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen("nf:event", (e) => {
        const env = e.payload as { topic?: string; payload?: { module?: string; state?: ModuleState } };
        if (env.topic !== "host.module_state") return;
        const id = env.payload?.module;
        const state = env.payload?.state;
        if (!id || !state) return;
        useModuleStatus.getState().apply(id, state);
      }),
    )
    .catch((err) => reportError(err, { context: "模块状态事件订阅失败", toast: false }));
}
