import { makeStyles } from "@fluentui/react-components";
import { isClipView, useSession } from "../../stores/session";
import HistorySection from "./panels/HistorySection";
import GroupsSection from "./panels/GroupsSection";
import StackSection from "./panels/StackSection";
import SecretSection from "./panels/SecretSection";
import SettingsSection from "./panels/SettingsSection";
import type { PanelProps } from "../../layout/panels";

/**
 * 剪切板面板壳（T-B3-1，09 §8.2）：五子面板按 session 键 clipView 一次只渲一个，
 * 选择入口在 SubNav 的「视图」维度（modules.ts 注册表），筛选六分组是另一维度、
 * 经 clipGroup 透传给历史区（两维度分键互不覆写，ProxyPanel 同型）。
 * 历史区逻辑（含 clipSearchParams 纯函数）整体迁至 panels/HistorySection.tsx。
 */
const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0, minWidth: 0 },
});

export default function ClipboardPanel({ search, group, onCounts }: PanelProps) {
  const styles = useStyles();
  // 持久化快照可能被手改成野值：渲染侧收窄兜 history（store 写侧同样过滤）
  const stored = useSession((s) => s.clipView);
  const view = isClipView(stored) ? stored : "history";
  return (
    <div className={styles.root}>
      {view === "history" && (
        <HistorySection search={search} group={group} onCounts={onCounts} />
      )}
      {view === "groups" && <GroupsSection />}
      {view === "stack" && <StackSection />}
      {view === "secret" && <SecretSection />}
      {view === "settings" && <SettingsSection />}
    </div>
  );
}
