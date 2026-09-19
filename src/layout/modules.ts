/** 模块交付阶段（决定导航分组与状态展示） */
export type ModulePhase = "P0" | "P1" | "P2";

/** 14 模块路由 id（D-29 B0/T-B0-1：联合类型是 PANELS/SUBNAV 穷尽性的编译期判据——
 * 新增模块 id 而注册表缺键时 tsc 直接报错，兜底三元无处藏身） */
export type ModuleId =
  | "clipboard"
  | "screenshot"
  | "ocr"
  | "proxy"
  | "vault"
  | "file"
  | "desktop"
  | "kvm"
  | "editor"
  | "notes"
  | "term"
  | "sys"
  | "automation"
  | "sync";

/** 模块清单（与 docs/DESIGN.md §3 一致，图标映射见 ModuleNav）。
 * 运行态不在此处（D-14）：真实状态唯一事实源是 stores/modules.ts 的事件流。 */
export interface ModuleDef {
  id: ModuleId;
  name: string;
  phase: ModulePhase;
}

/** 名称即 v1 承诺口径（D-08/D-09）：导航与标题不得出现录屏、PaddleOCR、翻译等 v1.1 能力 */
export const MODULES: ModuleDef[] = [
  { id: "clipboard", name: "剪切板中枢", phase: "P0" },
  { id: "screenshot", name: "截图与贴图", phase: "P0" },
  { id: "ocr", name: "OCR 文字识别", phase: "P0" },
  { id: "proxy", name: "代理与 VPN", phase: "P1" },
  { id: "vault", name: "安全与凭据", phase: "P1" },
  { id: "file", name: "文件与存储", phase: "P1" },
  { id: "desktop", name: "桌面效率", phase: "P1" },
  { id: "kvm", name: "键鼠共享", phase: "P1" },
  { id: "editor", name: "文本与 PDF", phase: "P2" },
  { id: "notes", name: "笔记与知识", phase: "P2" },
  { id: "term", name: "终端与运维", phase: "P2" },
  { id: "sys", name: "系统管理", phase: "P2" },
  { id: "automation", name: "自动化与拓展", phase: "P2" },
  { id: "sync", name: "跨设备同步", phase: "P2" },
];

export const MODULE_GROUPS: { label: string; phase: ModulePhase }[] = [
  { label: "核心", phase: "P0" },
  { label: "集成", phase: "P1" },
  { label: "扩展", phase: "P2" },
];

/** session/URL 里的模块 id 是字符串，进注册表查键前必须先收窄（穷尽性判据） */
export function isModuleId(id: string): id is ModuleId {
  return MODULES.some((m) => m.id === id);
}

/** 二级导航条目（panels/README 总则字面签名）：badgeKey 挂实时计数，缺省不显示数字 */
export interface SubNavItem {
  id: string;
  label: string;
  icon?: string;
  badgeKey?: string;
}

export interface SubNavSection {
  group: string;
  items: SubNavItem[];
}

/**
 * 全模块二级导航注册表（D-29 B0/T-B0-6，原 SubNav.tsx 里的 CLIP_GROUPS 硬编码迁址于此）。
 * 空数组 = 该模块暂无分组栏；面板档 §2 子面板 IA 落地时逐模块填充。
 */
export const SUBNAV: Record<ModuleId, SubNavSection[]> = {
  clipboard: [
    {
      group: "剪切板",
      items: [
        { id: "all", label: "全部", badgeKey: "all" },
        { id: "text", label: "文本", badgeKey: "text" },
        { id: "code", label: "代码", badgeKey: "code" },
        { id: "url", label: "链接", badgeKey: "url" },
        { id: "secret", label: "敏感", badgeKey: "secret" },
        { id: "files", label: "文件", badgeKey: "files" },
      ],
    },
  ],
  screenshot: [],
  ocr: [],
  proxy: [],
  vault: [],
  file: [],
  desktop: [],
  kvm: [],
  editor: [],
  notes: [],
  term: [],
  sys: [],
  automation: [],
  sync: [],
};
