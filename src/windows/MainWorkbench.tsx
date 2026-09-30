import { useCallback, useEffect, useState, Suspense } from "react";
import { makeStyles, tokens, Badge, Spinner } from "@fluentui/react-components";
import TitleBar from "../layout/TitleBar";
import Toolbar from "../layout/Toolbar";
import ModuleNav from "../layout/ModuleNav";
import SubNav from "../layout/SubNav";
import StatusBar from "../layout/StatusBar";
import MicaBackdrop from "../layout/MicaBackdrop";
import PanelHeader from "../components/PanelHeader";
import CapabilityCard from "../components/CapabilityCard";
import { capabilitiesFor } from "../layout/capabilities";
// PERF3（docs/impl/07）：路由级代码分割在 panels 注册表内声明——首屏只加载宿主框架 +
// 默认模块（ClipboardPanel），其余模块（含 Monaco/xterm 等重依赖）按需分 chunk；
// 子窗口（launcher/notebar/overlay）本就动态 import
// D-29 B0/T-B0-1：模块→组件与副标题一律走 PANELS/SUBNAV 注册表，禁止再写渲染三元
import { PANELS } from "../layout/panels";
import {
  MODULES,
  SUBNAV,
  isModuleId,
  subnavActive,
  subnavSelect,
  type SubnavStateKey,
  type SubNavScope,
} from "../layout/modules";
import { IN_TAURI } from "../ipc/env";
import { toggleLauncher } from "./launcherController";
import { toggleNoteBar } from "./notebarController";
import SchemaForm from "../settings/SchemaForm";
import HostSettings from "../settings/HostSettings";
import { toggleQuickPanel } from "./quickPanelController";
import { reportError } from "../stores/notifications";
import { useSession } from "../stores/session";
import { startModuleStatusFeed } from "../stores/modules";
import { startDesktopRemindFeed } from "../stores/desktopReminders";
import { startKvmPairAlertFeed } from "../stores/kvmPairAlerts";
import { startKvmClipAlertFeed } from "../stores/kvmClipAlerts";
import { startAutomationNotifyFeed } from "../stores/automationNotifications";
import { startModuleCrashAlertFeed } from "../stores/moduleCrashAlerts";

/**
 * 主工作台（像素真源＝docs/panels/2026-09-19/15-host-shell.md:81，
 * D-42 起网格 40/44/1fr/32 ＋ 228/190 双列导航，窄栏经 global.css 的 container query）。
 * 内容区为占位：剪切板真实列表在 U3 落地。
 */
const useStyles = makeStyles({
  app: {
    height: "100vh",
    display: "grid",
    gridTemplateRows: "40px 44px 1fr 32px",
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
    // D-43 C1：grid item 的自动最小尺寸＝min-content（overflow 为 visible 时生效），
    // 缺这一枚会让 work 按内容高生长、面板 overflowY 永不可滚（真机三径零位移实测）。
    minHeight: "0",
  },
  content: {
    display: "flex",
    flexDirection: "column",
    flex: 1,
    minWidth: 0,
    overflow: "hidden",
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
  // D-14：会话态（活跃模块/分组/搜索）入 session store，跨窗口与重启间保持一致
  const active = useSession((s) => s.activeModule);
  const setActive = useSession((s) => s.setActiveModule);
  // T-B0-4：设置中心跟随"最近激活的功能模块"（进设置不覆写该值）
  const settingsModule = useSession((s) => s.lastModule);
  const group = useSession((s) => s.clipGroup);
  const setGroup = useSession((s) => s.setClipGroup);
  // T-B2-3：代理 SUBNAV 项是"子面板"而非"分组"——选择态走 proxySubPanel 专键，
  // 与 clipGroup 分道（否则在代理页点「分流」会把剪切板回来后的筛选悄悄改掉）
  const proxySub = useSession((s) => s.proxySubPanel);
  const setProxySub = useSession((s) => s.setProxySubPanel);
  // T-B3-1：剪切板同理再分一键——「视图」维度（五子面板）与「筛选」维度（六分组）互不覆写
  const clipView = useSession((s) => s.clipView);
  const setClipView = useSession((s) => s.setClipView);
  // T-B5-8：同步五档再分一键（第三枚分键，理由同上）
  const syncSub = useSession((s) => s.syncSubPanel);
  const setSyncSub = useSession((s) => s.setSyncSubPanel);
  // T-B6-10 立档（T-B7-27 扩七档）：文件档选择态再分一键（第四枚分键，理由同上）
  const fileSub = useSession((s) => s.fileSubPanel);
  const setFileSub = useSession((s) => s.setFileSubPanel);
  const search = useSession((s) => s.clipSearch);
  const setSearch = useSession((s) => s.setClipSearch);
  const [counts, setCounts] = useState<Record<string, number>>({});
  // 稳定引用：防止 ClipboardPanel 的 load/refreshCounts 因回调重建而循环刷新
  const onCounts = useCallback((c: Record<string, number>) => setCounts(c), []);

  // 模块状态事实源：初始快照 + host.module_state 事件流（StatusBar/ModuleNav 共用）
  useEffect(() => {
    startModuleStatusFeed();
    // 到期提醒缓冲（T-B1-6）：订阅挂主窗口级，桌面面板未打开也不丢提醒
    startDesktopRemindFeed();
    // SEC-11（D-37 R-I1）：KVM 配对成功强提醒同样挂主窗口级
    startKvmPairAlertFeed();
    // D-42：对端推送剪贴板已写进本机剪切板，提醒挂主窗口级（KVM 面板未开也不静默）
    startKvmClipAlertFeed();
    // D-39①②：规则通知动作落点＋模块崩溃原因可见化（均主窗口级，面板未开也可见）
    startAutomationNotifyFeed();
    startModuleCrashAlertFeed();
  }, []);

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
            reportError(err, { context: "nf:event 处理异常", dedupeKey: "main-event-handler", toast: false });
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
        reportError(err, { context: "nf:event 监听注册失败", toast: false });
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

  // 持久化的会话值可能是历史/损坏 id：收窄失败确定性回落剪切板（首屏默认模块）
  const moduleId = isModuleId(active) ? active : "clipboard";
  // T-B3-1：选择态读写一律经 modules.ts 路由表（模块 × 维度 → session 键），
  // 取代原先按模块硬分叉的选择态三元
  const subnavSelections = { clipView, clipGroup: group, proxySub, syncSub, fileSub };
  const subnavSetters: Record<SubnavStateKey, (id: string) => void> = {
    clipView: setClipView,
    clipGroup: setGroup,
    proxySubPanel: setProxySub,
    syncSubPanel: setSyncSub,
    fileSubPanel: setFileSub,
  };
  const selectSubnav = (scope: SubNavScope, id: string) => {
    const sel = subnavSelect(moduleId, scope, id);
    if (sel) subnavSetters[sel.key](sel.value);
  };
  const current = MODULES.find((m) => m.id === moduleId);
  const def = PANELS[moduleId];
  const ModulePanel = def.panel;
  const isSettings = active === "__settings";

  return (
    <div className={styles.app}>
      <MicaBackdrop />
      <TitleBar />
      <Toolbar
        search={search}
        onSearchChange={setSearch}
        onQuickPanel={() => void toggleQuickPanel()}
        onLauncher={() => void toggleLauncher()}
        onScreenshot={() => void import("./overlayController").then((m) => m.startOverlay("shot"))}
      />

      <div className={styles.main}>
        <ModuleNav active={active} onChange={setActive} />
        <div className={styles.work} data-nf="work">
          {SUBNAV[moduleId].length > 0 && !isSettings && (
            <SubNav
              moduleId={moduleId}
              active={{
                view: subnavActive(moduleId, "view", subnavSelections),
                filter: subnavActive(moduleId, "filter", subnavSelections),
              }}
              onSelect={selectSubnav}
              counts={counts}
            />
          )}
          <section className={styles.content} aria-label="内容区">
            {isSettings ? (
              <>
                <PanelHeader
                  title={`设置中心 · ${MODULES.find((m) => m.id === settingsModule)?.name ?? "NexusForge"}`}
                  context="修改即校验即保存"
                  actions={<Badge appearance="outline">schema 驱动</Badge>}
                />
                {/* key=模块 id：切换跟随目标时整体重建，杜绝上一模块表单值闪现 */}
                <SchemaForm key={settingsModule} moduleId={settingsModule} />
                {/* 宿主段（托盘等，T-B7-11）：不随模块切换，常驻设置中心底部 */}
                <HostSettings />
              </>
            ) : (
              <>
                <PanelHeader
                  title={current?.name ?? "NexusForge"}
                  actions={<Badge appearance="outline">{current?.phase ?? "P0"}</Badge>}
                />
                {/* D-43 ③：能力句由壳层单点能力卡承载（context 槽的 nowrap+ellipsis 会把它裁成半句） */}
                <CapabilityCard capabilities={capabilitiesFor(moduleId)} />
                <Suspense fallback={<ModuleLoading />}>
                  <ModulePanel search={search} group={group} onCounts={onCounts} />
                </Suspense>
              </>
            )}
          </section>
        </div>
      </div>

      <StatusBar />
    </div>
  );
}
