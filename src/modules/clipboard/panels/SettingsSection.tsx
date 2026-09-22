import { useCallback, useEffect, useState } from "react";
import { makeStyles, Switch, Text, tokens } from "@fluentui/react-components";
import SchemaForm from "../../../settings/SchemaForm";
import {
  clipboardCaptureGet,
  clipboardCaptureSet,
  clipboardStats,
  parseAppError,
  type ClipCaptureState,
  type ClipStats,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { notify } from "../../../stores/notifications";

/**
 * 统计与设置子面板（T-B3-1 骨架 + T-B3-4 统计卡，细案 01§7.1 + 09 §8.1-⑪）：
 * 设置一律复用 SchemaForm（模块 config_schema 驱动，全仓唯一表单引擎，禁第二套），
 * 故剪贴板七项设置在侧栏「统计与设置」与设置中心同源同值。
 * T-B3-2 起顶部为「暂停捕获」专用卡：该键在 schema 中标 readOnly，
 * 通用表单不渲染它，clipboard_capture_set 因此是唯一 UI 写口（真源单点，缺陷⑦ 同律）。
 * 统计卡读 clipboard_stats（库内聚合，无估算）。
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
  statRow: {
    display: "flex",
    flexWrap: "wrap",
    gap: "4px 14px",
    marginTop: "6px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
  },
});

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function joinCounts(map: Record<string, number>): string {
  const entries = Object.entries(map).sort((a, b) => b[1] - a[1]);
  return entries.length === 0 ? "暂无" : entries.map(([k, v]) => `${k} ${v}`).join(" · ");
}

export default function SettingsSection() {
  const styles = useStyles();
  const [capture, setCapture] = useState<ClipCaptureState | null>(null);
  const [stats, setStats] = useState<ClipStats | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(() => {
    if (!IN_TAURI) return;
    clipboardCaptureGet()
      .then(setCapture)
      .catch((e) =>
        notify("error", "捕获状态读取失败", parseAppError(e)?.data.message ?? String(e)),
      );
    clipboardStats()
      .then(setStats)
      .catch((e) =>
        notify("error", "统计读取失败", parseAppError(e)?.data.message ?? String(e)),
      );
  }, []);

  useEffect(refresh, [refresh]);

  // 托盘/命令行改暂停、或分组写口改了统计口径后本卡要跟上（读运行时值而非盘值）
  useEffect(() => {
    if (!IN_TAURI) return;
    let unlisten: (() => void) | null = null;
    const topics = new Set(["clipboard.capture_state", "clipboard.groups_changed"]);
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          if (topics.has((e.payload as { topic?: string }).topic ?? "")) refresh();
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
      <div className={styles.card} style={{ alignItems: "flex-start" }}>
        <div className={styles.info}>
          <Text className={styles.name} block>
            库内统计
          </Text>
          <span className={styles.desc}>
            {stats === null
              ? "读取中…"
              : `共 ${stats.total} 条 · 占用 ${fmtBytes(stats.bytes_blob)}（内联正文 + blob 文件实长）`}
          </span>
          {stats && (
            <div className={styles.statRow}>
              <span>类型：{joinCounts(stats.by_content_type)}</span>
              <span>分组：{joinCounts(stats.by_group)}</span>
              <span>
                来源：
                {stats.top_source_apps.length === 0
                  ? "暂无"
                  : stats.top_source_apps.map(([app, n]) => `${app} ${n}`).join(" · ")}
              </span>
            </div>
          )}
        </div>
      </div>
      <SchemaForm moduleId="clipboard" />
    </div>
  );
}
