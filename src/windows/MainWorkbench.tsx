import { useCallback, useEffect, useState } from "react";
import { makeStyles, tokens, Text, Badge } from "@fluentui/react-components";
import TitleBar from "../layout/TitleBar";
import Toolbar from "../layout/Toolbar";
import ModuleNav from "../layout/ModuleNav";
import SubNav from "../layout/SubNav";
import StatusBar from "../layout/StatusBar";
import MicaBackdrop from "../layout/MicaBackdrop";
import ClipboardPanel from "../modules/clipboard/ClipboardPanel";
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
});

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
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic;
          if (topic === "clipboard.quick_panel_toggled") void toggleQuickPanel();
          if (topic === "screenshot.overlay_requested") {
            const mode = (e.payload as { payload?: { mode?: string } }).payload?.mode;
            void import("./overlayController").then((m) => m.startOverlay(mode === "ocr" ? "ocr" : "shot"));
          }
        }),
      )
      .then((u) => {
        unlisten = u;
      });
    return () => {
      unlisten?.();
    };
  }, []);

  // 启动恢复贴图（M3）
  useEffect(() => {
    if (!IN_TAURI) return;
    void import("./overlayController").then((m) => m.restorePins());
  }, []);

  const current = MODULES.find((m) => m.id === active);
  const isClipboard = active === "clipboard";
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
                      : `${current?.phase} 模块将在对应阶段交付`}
                  </span>
                </div>
                {isClipboard ? (
                  <ClipboardPanel search={search} group={group} onCounts={onCounts} />
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
              </>
            )}
          </section>
        </div>
      </div>

      <StatusBar />
    </div>
  );
}
