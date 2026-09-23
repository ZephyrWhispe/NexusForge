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
  /** 选择语义（T-B3-1）：view=切换子面板，filter=筛选同一清单；缺省 filter（proxy 六项零 churn） */
  scope?: SubNavScope;
}

/** 同一栏的两种选择维度（09 §8.1-⑩：clipboard 要同时有「视图 + 筛选」，不能再按模块硬分叉） */
export type SubNavScope = "view" | "filter";

export interface SubNavSection {
  group: string;
  items: SubNavItem[];
}

/**
 * 全模块二级导航注册表（D-29 B0/T-B0-6，原 SubNav.tsx 里的 CLIP_GROUPS 硬编码迁址于此）。
 * 空数组 = 该模块暂无分组栏；面板档 §2 子面板 IA 落地时逐模块填充。
 */
export const SUBNAV: Record<ModuleId, SubNavSection[]> = {
  // T-B3-1（09 §8.2）：剪切板两维度——「视图」五子面板（细案 01§2 IA）+「筛选」原六分组
  // （badgeKey 一项不丢，SubNav 角标语义不变），选择态分键落 clipView / clipGroup。
  clipboard: [
    {
      group: "视图",
      items: [
        { id: "history", label: "历史", scope: "view" },
        { id: "groups", label: "收藏与分组", scope: "view" },
        { id: "stack", label: "粘贴堆栈", scope: "view" },
        { id: "secret", label: "敏感库", scope: "view" },
        { id: "settings", label: "统计与设置", scope: "view" },
      ],
    },
    {
      group: "筛选",
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
  // T-B2-3（09 §5.2）：代理六子面板入口，id 与 session store proxySubPanel 联合类型一一对应
  // （缺省 scope="filter" 即路由表里的 proxySubPanel 键——代理只有一维，不占 view 维度）
  proxy: [
    {
      group: "代理",
      items: [
        { id: "overview", label: "总览" },
        { id: "nodes", label: "节点" },
        { id: "subs", label: "订阅" },
        { id: "rules", label: "分流" },
        { id: "kernel", label: "内核" },
        { id: "logs", label: "日志" },
      ],
    },
  ],
  vault: [],
  // T-B6-10（09 §6.2，panels/04 §2）：文件三档子面板，id 与 session store fileSubPanel 一一对应
  // （view 维度第四枚分键；网盘/treemap 等延后档不在此开栏——§6.3 归 DeferredBadge）
  file: [
    {
      group: "文件",
      items: [
        { id: "browse", label: "浏览与搜索", scope: "view" },
        { id: "transfers", label: "传输队列", scope: "view" },
        { id: "connections", label: "远程连接", scope: "view" },
      ],
    },
  ],
  desktop: [],
  kvm: [],
  editor: [],
  notes: [],
  term: [],
  sys: [],
  automation: [],
  // T-B5-8（09 §10.2）：同步五子面板入口，id 与 session store syncSubPanel 一一对应
  // （14-sync §2 五档；设置档不占子面板——它走设置中心，这里写文案指路）
  sync: [
    {
      group: "同步",
      items: [
        { id: "overview", label: "概览" },
        { id: "devices", label: "设备" },
        { id: "datasets", label: "数据集" },
        { id: "conflicts", label: "冲突" },
        { id: "activity", label: "活动" },
      ],
    },
  ],
};

/** 二级导航选择态的 session 键（MainWorkbench 的唯一派发目标，禁止再按模块写三元） */
export type SubnavStateKey =
  | "clipView"
  | "clipGroup"
  | "proxySubPanel"
  | "syncSubPanel"
  | "fileSubPanel";

/** 模块 × 维度 → session 键（09 §8.1-⑩ 红线：分组筛选与子面板切换两种语义不得互污，故分键） */
const SUBNAV_KEYS: Partial<Record<ModuleId, Partial<Record<SubNavScope, SubnavStateKey>>>> = {
  clipboard: { view: "clipView", filter: "clipGroup" },
  proxy: { filter: "proxySubPanel" },
  sync: { filter: "syncSubPanel" },
  file: { view: "fileSubPanel" },
};

/** 读选择态所需的最小会话快照（不依赖 store 类型，便于纯函数直测） */
export interface SubnavSelections {
  clipView: string;
  clipGroup: string;
  proxySub: string;
  syncSub: string;
  fileSub: string;
}

/** 该模块该维度当前高亮项 id；模块未在该维度注册选择态则 undefined */
export function subnavActive(
  moduleId: ModuleId,
  scope: SubNavScope,
  st: SubnavSelections,
): string | undefined {
  const key = SUBNAV_KEYS[moduleId]?.[scope];
  if (key === "clipView") return st.clipView;
  if (key === "clipGroup") return st.clipGroup;
  if (key === "proxySubPanel") return st.proxySub;
  if (key === "syncSubPanel") return st.syncSub;
  if (key === "fileSubPanel") return st.fileSub;
  return undefined;
}

/** 点击项 → 应写入的 session 键与值；项不在注册表或维度未注册选择态则 undefined（不写野值） */
export function subnavSelect(
  moduleId: ModuleId,
  scope: SubNavScope,
  id: string,
): { key: SubnavStateKey; value: string } | undefined {
  const key = SUBNAV_KEYS[moduleId]?.[scope];
  if (!key) return undefined;
  const registered = SUBNAV[moduleId].some((section) =>
    section.items.some((i) => (i.scope ?? "filter") === scope && i.id === id),
  );
  return registered ? { key, value: id } : undefined;
}
