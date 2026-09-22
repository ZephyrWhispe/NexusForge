import { useCallback, useEffect, useRef, useState, type ChangeEvent } from "react";
import {
  Badge,
  Button,
  Dropdown,
  Option,
  makeStyles,
  Text,
  Textarea,
  tokens,
} from "@fluentui/react-components";
import {
  ocrConfigGet,
  ocrCopyText,
  ocrEngineStatus,
  ocrRecognize,
  parseAppError,
  type EngineStatusDto,
  type OcrResultDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * OCR 识别主面板（D-29 B0/T-B0-3）：引擎状态卡 + 选图识别 + 分行结果 + 复制全部。
 * ocr_recognize 走 request 对象实签（commands/ocr.rs:10）；手选图片无 source_task_id，
 * 截图联动帧回填仍由覆盖层/事件通路负责，本面板不抢该语义。
 * 语言（T-B4-10）：面板多选只是**本次覆盖**，请求里留空即"跟随设置"——
 * 持久值经 ocr_config_get 只读显示，不在前端二次写入（单一真源）。
 * 结果诚实化（T-B4-13）：置信度列读 `engines_report_confidence`（引擎不报就显"未提供"），
 * 行末 [复制此块] 逐行走既有 ocr_copy_text；译文区有值才出现，失败原因摊开显示。
 */

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", flex: 1, minHeight: 0 },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    height: "40px",
    padding: "0 20px",
    flexShrink: 0,
  },
  spacer: { flex: 1 },
  body: { flex: 1, overflowY: "auto", padding: "8px 20px 20px" },
  sectionTitle: {
    display: "block",
    fontSize: tokens.fontSizeBase200,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground3,
    padding: "10px 0 6px",
  },
  engineRow: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "6px 10px",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
    marginBottom: "6px",
  },
  engineName: { fontWeight: tokens.fontWeightSemibold },
  engineId: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground4 },
  langs: {
    display: "block",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    padding: "2px 0 8px",
  },
  resultMeta: {
    display: "block",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    paddingBottom: "6px",
  },
  lineRow: {
    display: "flex",
    gap: "10px",
    alignItems: "baseline",
    padding: "3px 0",
    borderBottom: `1px solid ${tokens.colorNeutralStroke3}`,
  },
  lineText: { flex: 1, whiteSpace: "pre-wrap" },
  lineConf: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground4,
    fontVariantNumeric: "tabular-nums",
    flexShrink: 0,
  },
  errHint: {
    display: "block",
    padding: "4px 20px 12px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorPaletteDarkOrangeForeground1,
  },
  copyRow: { display: "flex", alignItems: "center", gap: "8px", padding: "8px 0" },
  note: {
    display: "block",
    padding: "4px 0 10px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorPaletteDarkOrangeForeground1,
  },
});

/** data URL → 纯 base64（去掉 "data:…;base64," 前缀；纯函数供测试） */
export function stripDataPrefix(dataUrl: string): string {
  const i = dataUrl.indexOf(",");
  return i >= 0 ? dataUrl.slice(i + 1) : dataUrl;
}

/** 置信度 → "98.4%"（钳位 0–100，一位小数；纯函数供测试） */
export function fmtConfidence(c: number): string {
  const pct = Math.max(0, Math.min(100, c * 100));
  return `${pct.toFixed(1)}%`;
}

/** 可用引擎计数徽标文本（纯函数供测试） */
export function engineBadge(s: EngineStatusDto | null): string {
  if (!s) return "引擎状态加载中…";
  const ok = s.engines.filter((e) => e.available).length;
  return `${ok}/${s.engines.length} 引擎可用`;
}

/** 置信度列文本（T-B4-13 红线）：引擎不报置信度时显示"未提供"，
 *  绝不把 win-ocr 的 1.0 占位渲染成"100.0%"——那是把"未知"说成"很有把握"。 */
export function confidence_reported(reported: boolean, conf: number): string {
  return reported ? fmtConfidence(conf) : "未提供";
}

/** 语言下拉占位：显示设置里的持久偏好（面板多选只是本次覆盖，纯函数供测试） */
export function langPlaceholder(cfgLangs: string[] | null): string {
  if (cfgLangs === null) return "语言：读取设置中…";
  return cfgLangs.length
    ? `语言：跟随设置（${cfgLangs.join(" · ")}）`
    : "语言：跟随设置（未设 · 引擎按系统语言自选）";
}

async function readFileB64(file: File): Promise<string> {
  const url = await new Promise<string>((res, rej) => {
    const r = new FileReader();
    r.onload = () => res(String(r.result));
    r.onerror = () => rej(new Error("读取文件失败"));
    r.readAsDataURL(file);
  });
  return stripDataPrefix(url);
}

export default function OcrPanel() {
  const styles = useStyles();
  const fileRef = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState<EngineStatusDto | null>(null);
  const [cfgLangs, setCfgLangs] = useState<string[] | null>(null);
  const [langs, setLangs] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<(OcrResultDto & { srcName: string }) | null>(null);
  const [err, setErr] = useState<{ message: string; engineMissing: boolean } | null>(null);

  const reloadStatus = useCallback(async () => {
    try {
      setStatus(await ocrEngineStatus());
    } catch (e) {
      reportError(e, { context: "引擎状态获取失败", dedupeKey: "ocr-status" });
    }
  }, []);

  useEffect(() => {
    void reloadStatus();
    // 运行态配置快照读失败不影响识别（只是占位文案退化为"未读取到设置"）
    void ocrConfigGet()
      .then((c) => setCfgLangs(c.langs))
      .catch((e) => reportError(e, { context: "OCR 设置读取失败", dedupeKey: "ocr-config" }));
  }, [reloadStatus]);

  const recognize = useCallback(
    async (file: File) => {
      setBusy(true);
      setErr(null);
      try {
        const imageB64 = await readFileB64(file);
        const r = await ocrRecognize({ image_b64: imageB64, langs });
        setResult({ ...r, srcName: file.name });
      } catch (e) {
        const app = parseAppError(e);
        setErr({
          message: app?.data.message ?? String(e),
          engineMissing: app?.data.code === "OCR_ENGINE_001",
        });
        setResult(null);
      } finally {
        setBusy(false);
      }
    },
    [langs],
  );

  const onFileChange = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const f = e.target.files?.[0];
      e.target.value = ""; // 允许连续选同一文件再次触发
      if (f) void recognize(f);
    },
    [recognize],
  );

  /** 走既有 ocr_copy_text（剪贴板回写窗口，不产生新历史条目）：全文与单行共用一入口 */
  const copy = useCallback(async (text: string) => {
    try {
      await ocrCopyText(text);
      notify("success", "已复制到剪贴板", "经剪贴板回写窗口写入，不会产生新历史条目");
    } catch (e) {
      reportError(e, { context: "复制失败", dedupeKey: "ocr-copy" });
    }
  }, []);

  const copyAll = useCallback(async () => {
    if (!result) return;
    await copy(result.text);
  }, [result, copy]);

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Button
          appearance="primary"
          size="small"
          disabled={busy}
          onClick={() => fileRef.current?.click()}
        >
          {busy ? "识别中…" : "选择图片识别"}
        </Button>
        <input
          ref={fileRef}
          type="file"
          accept="image/*"
          style={{ display: "none" }}
          onChange={onFileChange}
        />
        <Button size="small" onClick={() => void reloadStatus()}>
          刷新状态
        </Button>
        <Dropdown
          size="small"
          style={{ minWidth: "140px" }}
          placeholder={langPlaceholder(cfgLangs)}
          multiselect
          value={langs.join(", ")}
          selectedOptions={langs}
          onOptionSelect={(_, d) => {
            const opt = String(d.optionValue ?? "");
            setLangs((prev) =>
              prev.includes(opt) ? prev.filter((x) => x !== opt) : [...prev, opt],
            );
          }}
        >
          {(status?.languages ?? []).map((l) => (
            <Option key={l} value={l} text={l}>
              {l}
            </Option>
          ))}
        </Dropdown>
        <span className={styles.spacer} />
        <DeferredBadge label="PaddleOCR 引擎" decisionRef="D-08" />
        <Badge appearance="outline">{engineBadge(status)}</Badge>
      </div>
      {err?.engineMissing && (
        <Text className={styles.errHint}>
          先在下方引擎卡确认有『可用』引擎：Windows 需在『设置 → 时间和语言 → 语音』安装识别组件；
          装好后点『刷新状态』再试。
        </Text>
      )}
      <div className={styles.body}>
        <Text className={styles.sectionTitle}>引擎状态</Text>
        {!status ? (
          <EmptyState text="" loading />
        ) : (
          <>
            {status.engines.map((e) => (
              <div key={e.id} className={styles.engineRow}>
                <Text className={styles.engineName}>{e.name}</Text>
                <Text className={styles.engineId}>{e.id}</Text>
                <span className={styles.spacer} />
                <Badge
                  appearance="tint"
                  color={e.available ? "success" : "severe"}
                >
                  {e.available ? "可用" : "不可用"}
                </Badge>
              </div>
            ))}
            <Text className={styles.langs}>支持语言：{status.languages.join(" · ") || "（无）"}</Text>
          </>
        )}
        <Text className={styles.sectionTitle}>识别结果</Text>
        {busy ? (
          <EmptyState text="" loading />
        ) : err ? (
          <EmptyState text={`识别失败：${err.message}`} />
        ) : !result ? (
          <EmptyState text="尚未识别 · 点『选择图片识别』选取 PNG/JPG 截图，识别结果可一键复制" />
        ) : (
          <>
            <Text className={styles.resultMeta}>
              {result.srcName} · 引擎 {result.engine} · 语言 {result.lang} · {result.lines.length} 行
            </Text>
            {result.lines.map((l, i) => (
              <div key={i} className={styles.lineRow}>
                <Text className={styles.lineText}>{l.text}</Text>
                <Text className={styles.lineConf}>
                  {confidence_reported(result.engines_report_confidence, l.confidence)}
                </Text>
                <Button
                  size="small"
                  onClick={() => void copy(l.text)}
                  title="只复制本行文本（逐行各调一次 ocr_copy_text，不带邻行）"
                >
                  复制此块
                </Button>
              </div>
            ))}
            <div className={styles.copyRow}>
              <Button size="small" appearance="primary" onClick={() => void copyAll()}>
                复制全部
              </Button>
              <span className={styles.spacer} />
              <DeferredBadge label="翻译" decisionRef="D-08" />
            </div>
            <Textarea
              rows={6}
              value={result.text}
              onChange={(_, d) => setResult((p) => (p ? { ...p, text: d.value } : p))}
            />
            {/* 译文区：有值才渲染整节（null 时零空节），失败则把原因摊开而非静默 */}
            {result.translate ? (
              <>
                <Text className={styles.sectionTitle}>译文</Text>
                <Textarea rows={4} readOnly value={result.translate} />
              </>
            ) : result.translate_error ? (
              <Text className={styles.note}>译文未完成：{result.translate_error}</Text>
            ) : null}
          </>
        )}
      </div>
    </div>
  );
}
