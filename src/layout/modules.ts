/** 模块交付阶段（决定导航分组与状态展示） */
export type ModulePhase = "P0" | "P1" | "P2";

/** 13 模块清单（与 docs/DESIGN.md §3 一致，图标映射见 ModuleNav） */
export interface ModuleDef {
  id: string;
  name: string;
  phase: ModulePhase;
  /** P0 模块在阶段一有运行态演示 */
  running?: boolean;
}

export const MODULES: ModuleDef[] = [
  { id: "clipboard", name: "剪切板中枢", phase: "P0", running: true },
  { id: "screenshot", name: "截图与录屏", phase: "P0", running: true },
  { id: "ocr", name: "OCR 与翻译", phase: "P0", running: true },
  { id: "proxy", name: "代理与 VPN", phase: "P1" },
  { id: "vault", name: "安全与凭据", phase: "P1", running: true },
  { id: "file", name: "文件与存储", phase: "P1" },
  { id: "desktop", name: "桌面效率", phase: "P1" },
  { id: "kvm", name: "键鼠共享", phase: "P1", running: true },
  { id: "editor", name: "文本与 PDF", phase: "P2" },
  { id: "notes", name: "笔记与知识", phase: "P2" },
  { id: "term", name: "终端与运维", phase: "P2" },
  { id: "sys", name: "系统管理", phase: "P2" },
  { id: "automation", name: "自动化与拓展", phase: "P2" },
];

export const MODULE_GROUPS: { label: string; phase: ModulePhase }[] = [
  { label: "核心", phase: "P0" },
  { label: "集成", phase: "P1" },
  { label: "扩展", phase: "P2" },
];

/** 剪切板二级导航分组（docs/impl/02 C8；计数为 U3-3 接线前的占位） */
export const CLIP_GROUPS: { id: string; name: string; count: number }[] = [
  { id: "all", name: "全部", count: 0 },
  { id: "text", name: "文本", count: 0 },
  { id: "code", name: "代码", count: 0 },
  { id: "url", name: "链接", count: 0 },
  { id: "secret", name: "敏感", count: 0 },
  { id: "files", name: "文件", count: 0 },
];
