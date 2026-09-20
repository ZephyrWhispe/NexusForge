import { lazy, type ComponentType } from "react";
import { makeStyles, tokens, Text } from "@fluentui/react-components";
// PERF3（docs/impl/07）：剪切板为默认首屏，其余模块按需分 chunk
import ClipboardPanel from "../modules/clipboard/ClipboardPanel";
import { MODULES, type ModuleId } from "./modules";

/**
 * 全模块主面板注册表（D-29 B0/T-B0-1）：MainWorkbench 的 12 重渲染三元与
 * "模块界面待实现"字符串兜底就此删除——Record<ModuleId, …> 的穷尽性由 tsc 保证，
 * 新模块 id 缺面板时编译失败，不可能再静默落到兜底文案。
 */

/** 面板共享入参：仅剪切板消费（会话态透传 + 分组计数回传），其余面板忽略 */
export interface PanelProps {
  search: string;
  group: string;
  onCounts: (counts: Record<string, number>) => void;
}

export interface PanelDef {
  panel: ComponentType<PanelProps>;
  subtitle: string;
}

const useStyles = makeStyles({
  empty: {
    flex: 1,
    display: "grid",
    placeItems: "center",
    textAlign: "center",
    color: tokens.colorNeutralForeground3,
    padding: "0 24px",
  },
});

/** 显式占位（差距登记可见化轨道）：面板未落地时指名道姓给出细案文档去处，而非假装有内容 */
function ModulePlaceholder({ moduleId, planDoc }: { moduleId: ModuleId; planDoc: string }) {
  const styles = useStyles();
  const name = MODULES.find((m) => m.id === moduleId)?.name ?? moduleId;
  return (
    <div className={styles.empty}>
      <div>
        <Text size={400} weight="semibold" block>
          {name} · 面板施工中
        </Text>
        <Text size={300} block style={{ marginTop: "8px" }}>
          实施细案：docs/panels/2026-09-19/{planDoc}.md（批次排期见 docs/impl/09-blueprint-alignment.md）
        </Text>
      </div>
    </div>
  );
}

/** 供未来新模块在面板落地前显式挂占位（穷尽性由 Record 保证，禁默认兜底） */
export function placeholderFor(moduleId: ModuleId, planDoc: string): PanelDef["panel"] {
  return function Placeholder() {
    return <ModulePlaceholder moduleId={moduleId} planDoc={planDoc} />;
  };
}

const ScreenshotPanel = lazy(() => import("../modules/screenshot/ScreenshotPanel"));
const OcrPanel = lazy(() => import("../modules/ocr/OcrPanel"));
const KvmPanel = lazy(() => import("../modules/kvm/KvmPanel"));
const VaultPanel = lazy(() => import("../modules/vault/VaultPanel"));
const FilePanel = lazy(() => import("../modules/file/FilePanel"));
const ProxyPanel = lazy(() => import("../modules/proxy/ProxyPanel"));
const DesktopPanel = lazy(() => import("../modules/desktop/DesktopPanel"));
const EditorPanel = lazy(() => import("../modules/editor/EditorPanel"));
const NotesPanel = lazy(() => import("../modules/notes/NotesPanel"));
const TerminalPanel = lazy(() => import("../modules/term/TerminalPanel"));
const SysPanel = lazy(() => import("../modules/sys/SysPanel"));
const RulesPanel = lazy(() => import("../modules/automation/RulesPanel"));
const SyncPanel = lazy(() => import("../modules/sync/SyncPanel"));

export const PANELS: Record<ModuleId, PanelDef> = {
  clipboard: {
    panel: ClipboardPanel,
    subtitle: "历史 · 保留 30 天 · 敏感数据已加密（AES-256-GCM 信封）",
  },
  screenshot: {
    panel: ScreenshotPanel,
    subtitle: "框选截取 · 贴图 · 历史（真缩略图 · 一键再复制）",
  },
  ocr: { panel: OcrPanel, subtitle: "识别 · 引擎状态 · 分行结果 · 一键复制" },
  proxy: {
    panel: ProxyPanel,
    subtitle: "系统代理/TUN · 多内核框架（当前已注册 sing-box） · 订阅解析 · 崩溃自动还原",
  },
  vault: { panel: VaultPanel, subtitle: "Argon2id 信封 · AES-256-GCM 条目 · TOTP" },
  file: {
    panel: FilePanel,
    subtitle: "浏览 · 操作队列（断点续传） · 冲突策略 · 搜索 · 批量重命名",
  },
  desktop: {
    panel: DesktopPanel,
    subtitle: "快速启动器（Alt+Q） · 速记（Ctrl+Alt+N） · 桌面整理",
  },
  kvm: { panel: KvmPanel, subtitle: "发现 · 配对 · 会话 · 边缘切换（TCP+UDP+X25519）" },
  editor: {
    panel: EditorPanel,
    subtitle: "Monaco 编辑器 · 编码检测 · Markdown 预览 · PDF 工具",
  },
  notes: { panel: NotesPanel, subtitle: "Markdown 库 · [[双链]] 反链 · SM-2 复习 · 自由画布" },
  term: { panel: TerminalPanel, subtitle: "ConPTY 终端 · WSL · SSH/SFTP（TOFU） · Docker" },
  sys: { panel: SysPanel, subtitle: "资源监控 · 系统清理（白名单） · winget/scoop/choco" },
  automation: {
    panel: RulesPanel,
    subtitle: "事件/定时规则 · 受限条件求值 · 死信重放 · 防风暴冷却",
  },
  sync: { panel: SyncPanel, subtitle: "局域网 P2P · 复用配对信任根 · E2E 加密 · LWW 冲突" },
};

/** 穷尽性判定的纯函数（正例对 PANELS 恒空；负例测试靠它证明断言可红） */
export function missingPanelKeys(panels: Partial<Record<ModuleId, unknown>>): ModuleId[] {
  return MODULES.filter((m) => !(m.id in panels)).map((m) => m.id);
}
