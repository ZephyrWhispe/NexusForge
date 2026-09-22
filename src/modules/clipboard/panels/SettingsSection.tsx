import { useCallback, useEffect, useState } from "react";
import { makeStyles, Switch, Text, tokens } from "@fluentui/react-components";
import SchemaForm from "../../../settings/SchemaForm";
import { clipboardCaptureGet, clipboardCaptureSet, parseAppError, type ClipCaptureState } from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify } from "../../../stores/notifications";

/**
 * 统计与设置子面板（T-B3-1 骨架，细案 01§7.1 + 09 §8.1-⑪）：
 * 设置一律复用 SchemaForm（模块 config_schema 驱动，全仓唯一表单引擎，禁第二套），
 * 故剪贴板七项设置在侧栏「统计与设置」与设置中心同源同值。
 * T-B3-2 起顶部为「暂停捕获」专用卡：该键在 schema 中标 readOnly，
 * 通用表单不渲染它，clipboard_capture_set 因此是唯一 UI 写口（真源单点，缺陷⑦ 同律）。
 */
const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  card: {
    display: "flex",
    alignItems: "center",
    gap: "16px",
    margin: "12px 24px 0",
    padding: "12px 14px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground2,
    flexShrink: 0,
  },
  info: { flex: 1, minWidth: 0 },
  name: { display: "block", fontWeight: tokens.fontWeightSemibold },
  desc: { display: "block", fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground3 },
  pausedBadge: { color: tokens.colorPaletteDarkOrangeForeground1 },
});

export default function SettingsSection() {
  const styles = useStyles();
  const [capture, setCapture] = useState<ClipCaptureState | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardCaptureGet()
      .then(setCapture)
      .catch((e) =>
        notify("error", "捕获状态读取失败", parseAppError(e)?.data.message ?? String(e)),
      );
  }, []);

  useEffect(refresh, [refresh]);

  // 托盘/命令行改暂停后本卡要跟上（与历史区横幅同一事件源，读运行时值而非盘值）
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          if ((e.payload as { topic?: string }).topic === "clipboard.capture_state") refresh();
        }),
      )
      .then((u) => {
        unlisten = u;
      });
    return () => {
      unlisten?.();
    };
  }, [refresh]);

  const toggle = async (paused: boolean) => {
    setBusy(true);
    try {
      setCapture(await clipboardCaptureSet(paused));
    } catch (e) {
      notify("error", "切换捕获失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.card}>
        <div className={styles.info}>
          <Text className={styles.name} block>
            暂停捕获
          </Text>
          <span className={styles.desc}>
            {capture === null
              ? "读取中…"
              : capture.paused
                ? `已暂停：新复制内容不入库${capture.skipped > 0 ? `（暂停期间已跳过 ${capture.skipped} 次复制）` : ""}`
                : "运行中：复制内容实时入库"}
          </span>
        </div>
        <Switch
          checked={capture?.paused === true}
          disabled={busy}
          onChange={(_, d) => void toggle(d.checked)}
          label="暂停"
          style={{ alignSelf: "flex-start" }}
        />
      </div>
      <SchemaForm moduleId="clipboard" />
    </div>
  );
}
