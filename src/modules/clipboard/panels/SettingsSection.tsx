import { useCallback, useEffect, useState } from "react";
import {
  Button,
  makeStyles,
  Switch,
  Text,
  Textarea,
  tokens,
} from "@fluentui/react-components";
import SchemaForm from "../../../settings/SchemaForm";
import {
  clipboardCaptureGet,
  clipboardCaptureSet,
  clipboardStats,
  hostConfigGet,
  hostConfigSet,
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
 * T-B3-7 增「内容屏蔽规则」卡：一行一条正则，保存走 host_config_set 的**读-改-写**——
 * 只提交这一个键会把它连同其余八键一起写成缺省值，故先取回盘上全量再合并。
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
  blockCol: { display: "flex", flexDirection: "column", gap: "6px" },
  ops: { display: "flex", gap: "6px" },
});

/** 一行一条 → 去空白去空行（保留输入顺序：规则按序短路，用户看得见的顺序就是生效的顺序） */
function parseRuleLines(raw: string): string[] {
  return raw
    .split("\n")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

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
  /** 盘上全量配置（读-改-写的"读"侧）：null = 尚未取到，此时屏蔽卡不给保存 */
  const [clipCfg, setClipCfg] = useState<Record<string, unknown> | null>(null);
  const [ruleDraft, setRuleDraft] = useState("");
  const [ruleBusy, setRuleBusy] = useState(false);

  useEffect(() => {
    if (!IN_TAURI) return;
    hostConfigGet("clipboard")
      .then((cfg) => {
        setClipCfg(cfg);
        const patterns = Array.isArray(cfg.block_patterns)
          ? cfg.block_patterns.filter((x): x is string => typeof x === "string")
          : [];
        setRuleDraft(patterns.join("\n"));
      })
      .catch((e) =>
        notify("error", "屏蔽规则读取失败", parseAppError(e)?.data.message ?? String(e)),
      );
  }, []);

  const saveRules = async () => {
    if (!clipCfg) return;
    const lines = parseRuleLines(ruleDraft);
    setRuleBusy(true);
    try {
      // 展开既有全量再覆写单键：host_config_set 是整份替换语义，漏键即写缺省值
      await hostConfigSet("clipboard", { ...clipCfg, block_patterns: lines });
      setClipCfg({ ...clipCfg, block_patterns: lines });
      setRuleDraft(lines.join("\n"));
      notify("success", "已保存内容屏蔽规则", `${lines.length} 条规则即时生效（仅文本捕获）`);
    } catch (e) {
      notify("error", "保存屏蔽规则失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setRuleBusy(false);
    }
  };

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
      <div className={styles.card} style={{ alignItems: "flex-start" }}>
        <div className={styles.blockCol}>
          <div>
            <Text className={styles.name} block>
              内容屏蔽规则
            </Text>
            <span className={styles.desc}>
              一行一条正则，命中即整条不入库、也不发通知（例：验证码
              {String.raw`\d{6}$`}、卡号）。仅作用于文本捕获：图片与文件条目不受本表影响；
              写坏的正则只作废它自己。
            </span>
          </div>
          <Textarea
            rows={4}
            aria-label="内容屏蔽规则"
            value={ruleDraft}
            disabled={clipCfg === null}
            onChange={(_, d) => setRuleDraft(d.value)}
            placeholder={String.raw`\d{6}$`}
          />
          <div className={styles.ops}>
            <Button
              size="small"
              appearance="primary"
              disabled={clipCfg === null || ruleBusy}
              onClick={() => void saveRules()}
            >
              保存规则
            </Button>
          </div>
        </div>
      </div>
      <SchemaForm moduleId="clipboard" />
    </div>
  );
}
