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

/** 代理子面板 id（T-B2-3，09 §5.2）：与 SUBNAV[proxy] 六项一一对应 */
export type ProxySubPanel = "overview" | "nodes" | "subs" | "rules" | "kernel" | "logs";

const PROXY_SUB_PANEL_IDS: readonly string[] = [
  "overview",
  "nodes",
  "subs",
  "rules",
  "kernel",
  "logs",
];

export function isProxySubPanel(v: string): v is ProxySubPanel {
  return PROXY_SUB_PANEL_IDS.includes(v);
}

/** 同步子面板 id（T-B5-8，09 §10.2）：与 SUBNAV[sync] 五项一一对应
 *  （14-sync §2 五档：概览/设备/数据集/冲突/活动；设置档走设置中心不占子面板） */
export type SyncSubPanel = "overview" | "devices" | "datasets" | "conflicts" | "activity";

const SYNC_SUB_PANEL_IDS: readonly string[] = [
  "overview",
  "devices",
  "datasets",
  "conflicts",
  "activity",
];

export function isSyncSubPanel(v: string): v is SyncSubPanel {
  return SYNC_SUB_PANEL_IDS.includes(v);
}

/** 文件子面板 id（T-B6-10 立三档，T-B7-27 扩七档，09 §6.3 档 j）：与 SUBNAV[file]
 *  七项一一对应（panels/04 §2 七档字面：文件/传输/搜索/批量工具/远程连接/网盘/设置）。
 *  旧 "browse" id 保留 ⇒ 已持久化的 "browse" 快照值零迁移、零失效。 */
export type FileSubPanel =
  | "browse"
  | "transfers"
  | "search"
  | "batch"
  | "connections"
  | "netdisk"
  | "settings";

const FILE_SUB_PANEL_IDS: readonly string[] = [
  "browse",
  "transfers",
  "search",
  "batch",
  "connections",
  "netdisk",
  "settings",
];

export function isFileSubPanel(v: string): v is FileSubPanel {
  return FILE_SUB_PANEL_IDS.includes(v);
}

/** 剪切板子面板 id（T-B3-1，09 §8.2）：与 SUBNAV[clipboard] 「视图」五项一一对应
 *  （细案 01§2 IA：历史/收藏与分组/粘贴堆栈/敏感库/统计与设置） */
export type ClipView = "history" | "groups" | "stack" | "secret" | "settings";

const CLIP_VIEW_IDS: readonly string[] = [
  "history",
  "groups",
  "stack",
  "secret",
  "settings",
];

export function isClipView(v: string): v is ClipView {
  return CLIP_VIEW_IDS.includes(v);
}

interface SessionState {
  themeMode: ThemeMode;
  activeModule: string;
  /** 最近一次有效的功能模块 id（进设置中心时的跟随目标，__settings 不覆写它） */
  lastModule: ModuleId;
  clipGroup: string;
  clipSearch: string;
  /** 系统清理勾选态（T-B1-9）：目标 id 列表，跨会话持久；旧快照缺键由 merge 落默认 [] */
  sysCleanSelected: string[];
  /** 代理子面板选择态（T-B2-3）：与 clipGroup 分键——分组筛选与子面板切换两种语义不得互污；旧快照缺键回退 overview */
  proxySubPanel: ProxySubPanel;
  /** 剪切板子面板选择态（T-B3-1）：与 clipGroup 同为分键红线延续，旧快照缺键回退 history */
  clipView: ClipView;
  /** 同步子面板选择态（T-B5-8）：第三枚分键，旧快照缺键回退 overview */
  syncSubPanel: SyncSubPanel;
  /** 文件子面板选择态（T-B6-10）：第四枚分键，旧快照缺键回退 browse */
  fileSubPanel: FileSubPanel;
  setThemeMode: (mode: ThemeMode) => void;
  setActiveModule: (id: string) => void;
  setClipGroup: (group: string) => void;
  setClipSearch: (q: string) => void;
  setSysCleanSelected: (ids: string[]) => void;
  setProxySubPanel: (id: string) => void;
  setClipView: (id: string) => void;
  setSyncSubPanel: (id: string) => void;
  setFileSubPanel: (id: string) => void;
}

export const useSession = create<SessionState>()(
  persist(
    (set) => ({
      themeMode: "auto",
      activeModule: "clipboard",
      lastModule: "clipboard",
      clipGroup: "all",
      clipSearch: "",
      sysCleanSelected: [],
      proxySubPanel: "overview",
      clipView: "history",
      syncSubPanel: "overview",
      fileSubPanel: "browse",
      setThemeMode: (themeMode) => set({ themeMode }),
      setActiveModule: (id) =>
        set(isModuleId(id) ? { activeModule: id, lastModule: id } : { activeModule: id }),
      setClipGroup: (clipGroup) => set({ clipGroup }),
      setClipSearch: (clipSearch) => set({ clipSearch }),
      setSysCleanSelected: (sysCleanSelected) => set({ sysCleanSelected }),
      // 持久化快照可能被手改成野值：非注册 id 不落 store（面板侧恒有 overview 兜底）
      setProxySubPanel: (id) => set(isProxySubPanel(id) ? { proxySubPanel: id } : {}),
      // 同上（T-B3-1）：野值不落，面板渲染侧再经 isClipView 收窄兜 history
      setClipView: (id) => set(isClipView(id) ? { clipView: id } : {}),
      // 同上（T-B5-8）：第三枚分键——同步的五档与代理/剪切板的选择态互不覆写
      setSyncSubPanel: (id) => set(isSyncSubPanel(id) ? { syncSubPanel: id } : {}),
      // 同上（T-B6-10 立档，T-B7-27 扩七档）：第四枚分键——文件七档与其余模块选择态互不覆写
      setFileSubPanel: (id) => set(isFileSubPanel(id) ? { fileSubPanel: id } : {}),
    }),
    {
      name: "nf-session",
      partialize: (s) => ({
        themeMode: s.themeMode,
        activeModule: s.activeModule,
        lastModule: s.lastModule,
        clipGroup: s.clipGroup,
        sysCleanSelected: s.sysCleanSelected,
        proxySubPanel: s.proxySubPanel,
        clipView: s.clipView,
        syncSubPanel: s.syncSubPanel,
        fileSubPanel: s.fileSubPanel,
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
