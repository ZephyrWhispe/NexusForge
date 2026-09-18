import { useCallback, useEffect, useState, lazy, Suspense } from "react";
import { makeStyles, tokens, Text, Badge, Spinner } from "@fluentui/react-components";
import TitleBar from "../layout/TitleBar";
import Toolbar from "../layout/Toolbar";
import ModuleNav from "../layout/ModuleNav";
import SubNav from "../layout/SubNav";
import StatusBar from "../layout/StatusBar";
import MicaBackdrop from "../layout/MicaBackdrop";
// PERF3（docs/impl/07）：路由级代码分割——首屏只加载宿主框架 + 默认模块（ClipboardPanel），
// 其余模块（含 Monaco/xterm 等重依赖）按需分 chunk；子窗口（launcher/notebar/overlay）本就动态 import
import ClipboardPanel from "../modules/clipboard/ClipboardPanel";
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
import { toggleLauncher } from "./launcherController";
import { toggleNoteBar } from "./notebarController";
import SchemaForm from "../settings/SchemaForm";
import { MODULES } from "../layout/modules";
import { IN_TAURI } from "../ipc/env";
import { toggleQuickPanel } from "./quickPanelController";

/**
 * 主工作台（docs/DESIGN.md §3 像素级布局：40/44/1fr/28 四行 + 228/190 双列导航）。
 * 内容区为占位：剪切板真实列表在 U3 落地。
 */
const useStyles = makeStyles({
  app: {
    height: "100vh",
    display: "grid",
    gridTemplateRows: "40px 44px 1fr 28px",
    // U1-4：根背景透明，Mica（Tauri）或渐变回退（浏览器）由 MicaBackdrop 提供
    backgroundColor: "transparent",
    color: tokens.colorNeutralForeground1,
    overflow: "hidden",
  },
  main: {
    display: "grid",
    gridTemplateColumns: "228px 1fr",
    minHeight: "0",
  },
  work: {
    display: "flex",
    minWidth: "0",
  },
  content: {
    flex: 1,
    minWidth: 0,
    display: "flex",
    flexDirection: "column",
    overflow: "hidden",
  },
  head: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    padding: "14px 20px 10px",
  },
  headTitle: { fontSize: tokens.fontSizeBase500, fontWeight: tokens.fontWeightSemibold },
  meta: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  empty: {
    flex: 1,
    display: "grid",
    placeItems: "center",
    textAlign: "center",
    color: tokens.colorNeutralForeground3,
  },
  loading: {
    flex: 1,
    display: "grid",
    placeItems: "center",
  },
});

/** lazy 模块加载占位（PERF3 代码分割 fallback） */
function ModuleLoading() {
  const styles = useStyles();
  return (
    <div className={styles.loading} aria-label="模块加载中">
      <Spinner size="large" label="加载模块…" labelPosition="below" />
    </div>
  );
}

export default function MainWorkbench() {
  const styles = useStyles();
  const [active, setActive] = useState("clipboard");
  const [group, setGroup] = useState("all");
  const [search, setSearch] = useState("");
  const [counts, setCounts] = useState<Record<string, number>>({});
  // 稳定引用：防止 ClipboardPanel 的 load/refreshCounts 因回调重建而循环刷新
  const onCounts = useCallback((c: Record<string, number>) => setCounts(c), []);

  // U2-4/U3-5：全局快捷键 → OS → 事件 → 快速面板 / 截图覆盖层
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    // StrictMode 双挂载：第一次 effect 的 listen 完成前 cleanup 已执行，
    // cancelled 保证迟到的监听器被立即移除（否则泄漏为双发）
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          try {
            const topic = (e.payload as { topic?: string }).topic;
            if (topic === "clipboard.quick_panel_toggled") void toggleQuickPanel();
            if (topic === "screenshot.overlay_requested") {
              const mode = (e.payload as { payload?: { mode?: string } }).payload?.mode;
              void import("./overlayController").then((m) => m.startOverlay(mode === "ocr" ? "ocr" : "shot"));
            }
            if (topic === "desktop.launcher_toggled") void toggleLauncher();
            if (topic === "desktop.note_quick") void toggleNoteBar();
          } catch (err) {
            import("../ipc/client").then((c) => c.hostLog("error", `nf:event 处理异常: ${String(err)}`));
          }
        }),
      )
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
      })
      .catch((err) => {
        import("../ipc/client").then((c) => c.hostLog("error", `nf:event 监听注册失败: ${String(err)}`));
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // 启动恢复贴图 + 预热隐藏覆盖层窗口（M3，热键秒开）
  useEffect(() => {
    if (!IN_TAURI) return;
    void import("./overlayController").then((m) => {
      m.restorePins();
      m.prewarmOverlay();
    });
  }, []);

  const current = MODULES.find((m) => m.id === active);
  const isClipboard = active === "clipboard";
  const isKvm = active === "kvm";
  const isVault = active === "vault";
  const isFile = active === "file";
  const isProxy = active === "proxy";
  const isDesktop = active === "desktop";
  const isEditor = active === "editor";
  const isNotes = active === "notes";
  const isTerm = active === "term";
  const isSys = active === "sys";
  const isAutomation = active === "automation";
  const isSync = active === "sync";
  const isSettings = active === "__settings";

  return (
    <div className={styles.app}>
      <MicaBackdrop />
      <TitleBar />
      <Toolbar search={search} onSearchChange={setSearch} />

      <div className={styles.main}>
        <ModuleNav active={active} onChange={setActive} />
        <div className={styles.work}>
          {isClipboard && <SubNav active={group} onSelect={setGroup} counts={counts} />}
          <section className={styles.content} aria-label="内容区">
            {isSettings ? (
              <>
                <div className={styles.head}>
                  <span className={styles.headTitle}>设置中心</span>
                  <Badge appearance="outline">schema 驱动</Badge>
                  <span className={styles.meta}>修改即校验即保存</span>
                </div>
                <SchemaForm moduleId="clipboard" />
              </>
            ) : (
              <>
                <div className={styles.head}>
                  <span className={styles.headTitle}>{current?.name ?? "NexusForge"}</span>
                  <Badge appearance="outline">{current?.phase ?? "P0"}</Badge>
                  <span className={styles.meta}>
                    {isClipboard
                      ? "历史 · 保留 30 天 · 敏感数据已加密（DPAPI）"
                      : isKvm
                        ? "发现 · 配对 · 会话 · 边缘切换（TCP+UDP+X25519）"
                        : isVault
                          ? "Argon2id 信封 · AES-256-GCM 条目 · TOTP"
                          : isFile
                            ? "浏览 · 操作队列（断点续传） · 冲突策略 · 搜索 · 批量重命名"
                            : isProxy
                              ? "sing-box 内核 · 系统代理/TUN · 订阅解析 · 崩溃自动还原"
                              : isDesktop
                                ? "快速启动器（Alt+Q） · 速记（Ctrl+Alt+N） · 桌面整理"
                                : isEditor
                                  ? "Monaco 编辑器 · 编码检测 · Markdown 预览 · PDF 工具"
                                  : isNotes
                                    ? "Markdown 库 · [[双链]] 反链 · SM-2 复习 · 自由画布"
                                    : isTerm
                                      ? "ConPTY 终端 · WSL · SSH/SFTP（TOFU） · Docker"
                                      : isSys
                                        ? "资源监控 · 系统清理（白名单） · winget/scoop/choco"
                                        : isAutomation
                                          ? "事件/定时规则 · 受限条件求值 · 死信重放 · 防风暴冷却"
                                          : isSync
                                            ? "局域网 P2P · 复用配对信任根 · E2E 加密 · LWW 冲突"
                                            : `${current?.phase} 模块将在对应阶段交付`}
                  </span>
                </div>
                {isClipboard ? (
                  <ClipboardPanel search={search} group={group} onCounts={onCounts} />
                ) : (
                  <Suspense fallback={<ModuleLoading />}>
                    {isKvm ? (
                      <KvmPanel />
                    ) : isVault ? (
                      <VaultPanel />
                    ) : isFile ? (
                      <FilePanel />
                    ) : isProxy ? (
                      <ProxyPanel />
                    ) : isDesktop ? (
                      <DesktopPanel />
                    ) : isEditor ? (
                      <EditorPanel />
                    ) : isNotes ? (
                      <NotesPanel />
                    ) : isTerm ? (
                      <TerminalPanel />
                    ) : isSys ? (
                      <SysPanel />
                    ) : isAutomation ? (
                      <RulesPanel />
                    ) : isSync ? (
                      <SyncPanel />
                    ) : (
                      <div className={styles.empty}>
                        <div>
                          <Text size={400} weight="semibold" block>
                            模块界面待实现
                          </Text>
                          <Text size={300} block style={{ marginTop: "8px" }}>
                            架构与接口已定义于 docs/impl/ 对应文档
                          </Text>
                        </div>
                      </div>
                    )}
                  </Suspense>
                )}
              </>
            )}
          </section>
        </div>
      </div>

      <StatusBar />
    </div>
  );
}
