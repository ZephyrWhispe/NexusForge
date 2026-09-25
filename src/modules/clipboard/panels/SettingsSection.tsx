import { useCallback, useEffect, useState } from "react";
import {
  Button,
  Checkbox,
  Input,
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
  clipboardExport,
  clipboardImport,
  clipboardStats,
  hostConfigGet,
  hostConfigSet,
  parseAppError,
  type ClipCaptureState,
  type ClipExportResult,
  type ClipStats,
} from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import { confirmAction } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";

/**
 * 统计与设置子面板（T-B3-1 骨架 + T-B3-4 统计卡，细案 01§7.1 + 09 §8.1-⑪）：
 * 设置一律复用 SchemaForm（模块 config_schema 驱动，全仓唯一表单引擎，禁第二套），
 * 故剪贴板七项设置在侧栏「统计与设置」与设置中心同源同值。
 * T-B3-2 起顶部为「暂停捕获」专用卡：该键在 schema 中标 readOnly，
 * 通用表单不渲染它，clipboard_capture_set 因此是唯一 UI 写口（真源单点，缺陷⑦ 同律）。
 * T-B3-7 增「内容屏蔽规则」卡：一行一条正则，保存走 host_config_set 的**读-改-写**——
 * 只提交这一个键会把它连同其余八键一起写成缺省值，故先取回盘上全量再合并。
 * T-B3-9 增「备份导出」/「备份导入」两卡：口令信封在宿主侧，前端只递口令不碰明文；
 * 勾选「包含敏感条目」时口令不足 8 字符即内联拦下（不发命令），与宿主的
 * CLIPBOARD_EXPORT_001 是同一道门的两侧，不是两套标准。
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
  fieldCol: { display: "flex", flexDirection: "column", gap: "6px", maxWidth: "520px" },
  fieldRow: { display: "flex", gap: "6px", alignItems: "flex-end" },
  fieldError: { fontSize: tokens.fontSizeBase200, color: tokens.colorPaletteRedForeground1 },
  resultPath: {
    fontSize: tokens.fontSizeBase200,
    fontFamily: tokens.fontFamilyMonospace,
    wordBreak: "break-all",
  },
});

/** 与宿主 `clipboard_core::backup::MIN_PASSPHRASE` 同值：内联拦的是同一道门 */
const MIN_PASSPHRASE = 8;

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
  const [expPass, setExpPass] = useState("");
  const [expSecrets, setExpSecrets] = useState(false);
  const [expBusy, setExpBusy] = useState(false);
  const [expResult, setExpResult] = useState<ClipExportResult | null>(null);
  const [impPath, setImpPath] = useState("");
  const [impPass, setImpPass] = useState("");
  const [impBusy, setImpBusy] = useState(false);

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
    let cancelled = false;
    const topics = new Set(["clipboard.capture_state", "clipboard.groups_changed"]);
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          if (topics.has((e.payload as { topic?: string }).topic ?? "")) refresh();
        }),
      )
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      });
    return () => {
      cancelled = true;
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

  // 按码点数长度（与宿主 chars().count() 同口径）：中文口令不该被 UTF-8 字节数虚高放行
  const expChars = Array.from(expPass).length;
  const expTooShort = expSecrets && expChars < MIN_PASSPHRASE;

  const doExport = async () => {
    if (expTooShort || expBusy) return;
    setExpBusy(true);
    try {
      const res = await clipboardExport(expPass, expSecrets);
      setExpResult(res);
      notify(
        "success",
        "备份已导出",
        `${res.entries} 条（含敏感 ${res.secrets} 条${res.images_skipped ? `，图片/blob 外置 ${res.images_skipped} 条未随文件` : ""}）`,
      );
    } catch (e) {
      notify("error", "导出失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setExpBusy(false);
    }
  };

  const copyPath = async () => {
    if (!expResult) return;
    try {
      await navigator.clipboard.writeText(expResult.path);
      notify("success", "已复制备份路径");
    } catch (e) {
      notify("error", "复制失败", parseAppError(e)?.data.message ?? String(e));
    }
  };

  const doImport = async () => {
    if (impBusy) return;
    const ok = await confirmAction({
      title: "导入备份",
      impact: [`从「${impPath}」读入并与当前库合并`],
      detail:
        "口令错误或文件被篡改时一行都不落；已存在的条目按内容哈希去重，不会凭空多出第二份。",
      confirmLabel: "导入",
      danger: false,
    });
    if (!ok) return;
    setImpBusy(true);
    try {
      const r = await clipboardImport(impPath, impPass);
      notify(
        "success",
        "备份已导入",
        `新增 ${r.imported} 条 · 重复 ${r.duplicates} 条 · 敏感 ${r.secrets} 条`,
      );
      refresh();
    } catch (e) {
      notify("error", "导入失败", parseAppError(e)?.data.message ?? String(e));
    } finally {
      setImpBusy(false);
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
      <div className={styles.card} style={{ alignItems: "flex-start" }}>
        <div className={styles.blockCol}>
          <div>
            <Text className={styles.name} block>
              备份导出
            </Text>
            <span className={styles.desc}>
              口令派生密钥（argon2id）后整包 AES-256-GCM 加密，落在
              {' {应用数据}/export/ '}
              下；文件里既没有明文正文也没有明文口令，口令丢了这份备份就解不开（没有后门）。
            </span>
          </div>
          <div className={styles.fieldRow}>
            <span aria-hidden="true">🔒</span>
            <Input
              type="password"
              appearance="underline"
              aria-label="备份口令"
              placeholder={`口令（含敏感条目时至少 ${MIN_PASSPHRASE} 字符）`}
              value={expPass}
              onChange={(_, d) => setExpPass(d.value)}
              style={{ flex: 1 }}
            />
            <Button
              size="small"
              appearance="primary"
              disabled={expBusy || !IN_TAURI || expTooShort}
              onClick={() => void doExport()}
            >
              {expBusy ? "导出中…" : "导出备份"}
            </Button>
          </div>
          <Checkbox
            checked={expSecrets}
            disabled={expBusy}
            onChange={(_, d) => setExpSecrets(d.checked === true)}
            label="包含敏感条目（解出后重新封入口令信封）"
          />
          {expTooShort && (
            <span className={styles.fieldError}>
              勾选了「包含敏感条目」，口令须至少 {MIN_PASSPHRASE} 字符——短口令等于把这道门让出去。
            </span>
          )}
          {expResult && (
            <div className={styles.fieldRow}>
              <span className={styles.resultPath}>{expResult.path}</span>
              <Button size="small" onClick={() => void copyPath()}>
                复制路径
              </Button>
            </div>
          )}
        </div>
      </div>
      <div className={styles.card} style={{ alignItems: "flex-start" }}>
        <div className={styles.blockCol}>
          <div>
            <Text className={styles.name} block>
              备份导入
            </Text>
            <span className={styles.desc}>
              填本程序导出的 .nfclip.json 路径与口令：认证通过才逐行合并入库，
              口令错/文件被改都在落库之前整口拒，不会留下半套数据。
            </span>
          </div>
          <Input
            aria-label="备份文件路径"
            placeholder="C:\\Users\\...\\export\\clipboard-….nfclip.json"
            value={impPath}
            onChange={(_, d) => setImpPath(d.value)}
          />
          <div className={styles.fieldRow}>
            <span aria-hidden="true">🔑</span>
            <Input
              type="password"
              appearance="underline"
              aria-label="导入口令"
              placeholder="口令"
              value={impPass}
              onChange={(_, d) => setImpPass(d.value)}
              style={{ flex: 1 }}
            />
            <Button
              size="small"
              disabled={impBusy || !IN_TAURI || impPath.trim().length === 0}
              onClick={() => void doImport()}
            >
              {impBusy ? "导入中…" : "导入备份"}
            </Button>
          </div>
        </div>
      </div>
      <SchemaForm moduleId="clipboard" />
    </div>
  );
}
