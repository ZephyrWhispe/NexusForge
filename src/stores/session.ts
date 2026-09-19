import { create } from "zustand";
import { persist } from "zustand/middleware";
import { isModuleId, type ModuleId } from "../layout/modules";

/**
 * 会话 store（docs/DESIGN.md §7 Zustand 基线，审查 D-14）：
 * 主题模式 / 活跃模块 / 剪切板分组与搜索词。
 * persist 到 localStorage：同源的多窗口（?w=launcher/quickpanel/…）经 storage
 * 事件实时互相同步——主题切换跨窗口一致即由此保证（OS 深浅色变化另由
 * main.tsx 的 matchMedia 监听器逐窗口响应）。搜索词为会话内易失状态，不入持久化。
 */

export type ThemeMode = "auto" | "light" | "dark";

interface SessionState {
  themeMode: ThemeMode;
  activeModule: string;
  /** 最近一次有效的功能模块 id（进设置中心时的跟随目标，__settings 不覆写它） */
  lastModule: ModuleId;
  clipGroup: string;
  clipSearch: string;
  setThemeMode: (mode: ThemeMode) => void;
  setActiveModule: (id: string) => void;
  setClipGroup: (group: string) => void;
  setClipSearch: (q: string) => void;
}

export const useSession = create<SessionState>()(
  persist(
    (set) => ({
      themeMode: "auto",
      activeModule: "clipboard",
      lastModule: "clipboard",
      clipGroup: "all",
      clipSearch: "",
      setThemeMode: (themeMode) => set({ themeMode }),
      setActiveModule: (id) =>
        set(isModuleId(id) ? { activeModule: id, lastModule: id } : { activeModule: id }),
      setClipGroup: (clipGroup) => set({ clipGroup }),
      setClipSearch: (clipSearch) => set({ clipSearch }),
    }),
    {
      name: "nf-session",
      partialize: (s) => ({
        themeMode: s.themeMode,
        activeModule: s.activeModule,
        lastModule: s.lastModule,
        clipGroup: s.clipGroup,
      }),
    },
  ),
);

/** 解析生效的明暗：显式模式优先，auto 跟随系统 */
export function resolveIsLight(mode: ThemeMode, systemPrefersLight: boolean): boolean {
  return mode === "light" ? true : mode === "dark" ? false : systemPrefersLight;
}

// zustand persist 只在初始化时水合；跨窗口同步需自行响应 storage 事件
//（Tauri 多窗口同源，写方窗口不触发、其他窗口触发 → rehydrate 即跨窗口实时同步主题/活跃模块）
window.addEventListener("storage", (e) => {
  if (e.key !== "nf-session") return;
  void useSession.persist.rehydrate();
});
