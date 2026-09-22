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
  ocrExport,
  ocrRecognize,
  parseAppError,
  type EngineStatusDto,
  type OcrExportFormat,
  type OcrResultDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * OCR 识别主面板（D-29 B0/T-B0-3）：引擎状态卡 + 选图识别（可多选成批）+ 分行结果 + 复制全部。
 * ocr_recognize 走 request 对象实签（commands/ocr.rs:10）；手选图片无 source_task_id，
 * 截图联动帧回填仍由覆盖层/事件通路负责，本面板不抢该语义。
 * 语言（T-B4-10）：面板多选只是**本次覆盖**，请求里留空即"跟随设置"——
 * 持久值经 ocr_config_get 只读显示，不在前端二次写入（单一真源）。
 * 结果诚实化（T-B4-13）：置信度列读 `engines_report_confidence`（引擎不报就显"未提供"），
 * 行末 [复制此块] 逐行走既有 ocr_copy_text；译文区有值才出现，失败原因摊开显示。
 * 批量与导出（T-B4-12 / §9.1-⑮）：`<input multiple>` 在浏览器侧读字节、**串行**逐张走既有
 * ocr_recognize（零新读盘命令，原生窗口级拖放因此登记为收窄项）；单张失败只记该行并继续，
 * 合并文本导出交给后端拼路径（前端零目录入参）。
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
  queueRow: {
    display: "flex",
    gap: "10px",
    alignItems: "center",
    padding: "3px 0",
    borderBottom: `1px solid ${tokens.colorNeutralStroke3}`,
  },
  queueName: {
    flex: 1,
    minWidth: 0,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  queueMeta: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground4,
    fontVariantNumeric: "tabular-nums",
    flexShrink: 0,
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

/** 队列里的一张图（T-B4-12）：pending 行不进气泡文案也不进导出，避免把"还没跑完"说成"没字" */
export interface OcrQueueItem {
  name: string;
  size: number;
  status: "pending" | "ok" | "fail";
  chars: number;
  reason?: string;
  result?: OcrResultDto;
}

/** 队列状态徽标文本（纯函数供测试） */
export function queueStatusBadge(s: OcrQueueItem["status"]): string {
  return s === "ok" ? "已识别" : s === "fail" ? "失败" : "排队中";
}

/**
 * 合并队列文本（纯函数供测试）。红线：**不静默丢文件**——失败行同样成节，
 * 节里写"识别失败：原因"，用户读导出文件时看得见哪个文件没出字。
 * 节间空行分隔；txt 用 `===== 名 =====`，md 用二级标题 + 围栏代码块。
 */
export function mergeOcrTexts(items: OcrQueueItem[], format: OcrExportFormat): string {
  const settled = items.filter((it) => it.status !== "pending");
  return settled
    .map((it) => {
      const body =
        it.status === "fail" ? `识别失败：${it.reason ?? "未知原因"}` : (it.result?.text ?? "");
      return format === "txt"
        ? `===== ${it.name} =====\n${body}`
        : `## ${it.name}\n\n\`\`\`\n${body}\n\`\`\``;
    })
    .join("\n\n");
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
  const [queue, setQueue] = useState<OcrQueueItem[]>([]);
  const [exporting, setExporting] = useState<OcrExportFormat | null>(null);
  /** 批次令牌：清空队列即自增作废在途批次（循环下一轮见令牌不匹配就停手，零新调用） */
  const runIdRef = useRef(0);

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

  /**
   * 串行跑完一批（T-B4-12）：同一时刻只有一次 `ocr_recognize` 在途——两张图并发打两个
   * 引擎（或打同一个引擎两次）都是无谓的内存尖峰，且失败归因会交错。
   * 单张失败只记进该行并继续（红线：整批不中断），全批皆败才 reportError 一次。
   */
  const runBatch = useCallback(
    async (files: File[]) => {
      if (files.length === 0) return;
      const runId = runIdRef.current + 1;
      runIdRef.current = runId;
      setErr(null);
      setBusy(true);
      setQueue(
        files.map((f) => ({
          name: f.name,
          size: f.size,
          status: "pending" as const,
          chars: 0,
        })),
      );
      let failed = 0;
      let lastCause: unknown = null;
      let lastReason: { message: string; engineMissing: boolean } | null = null;
      for (let i = 0; i < files.length; i += 1) {
        // 队列已被清空或新批次接手：本批就此作废，不再发起任何识别调用
        if (runIdRef.current !== runId) return;
        const file = files[i];
        try {
          const imageB64 = await readFileB64(file);
          const r = await ocrRecognize({ image_b64: imageB64, langs });
          setResult({ ...r, srcName: file.name });
          setQueue((q) =>
            q.map((it, idx) =>
              idx === i ? { ...it, status: "ok", chars: r.text.length, result: r } : it,
            ),
          );
        } catch (e) {
          const app = parseAppError(e);
          const reason = app?.data.message ?? String(e);
          failed += 1;
          lastCause = e;
          lastReason = { message: reason, engineMissing: app?.data.code === "OCR_ENGINE_001" };
          setQueue((q) =>
            q.map((it, idx) => (idx === i ? { ...it, status: "fail", reason } : it)),
          );
        }
      }
      if (runIdRef.current === runId) {
        setBusy(false);
        if (failed === files.length && lastReason) {
          // 整批皆败才升级到面板错误 + 一次上报：部分失败的信息已经在队列行里，
          // 逐张 reportError 只会把同一个真因刷成 N 条噪音
          setErr(lastReason);
          setResult(null);
          reportError(lastCause, { context: "识别失败", dedupeKey: "ocr-batch" });
        }
      }
    },
    [langs],
  );

  const onFileChange = useCallback(
    (e: ChangeEvent<HTMLInputElement>) => {
      const files = Array.from(e.target.files ?? []);
      e.target.value = ""; // 允许连续选同一批文件再次触发
      if (files.length) void runBatch(files);
    },
    [runBatch],
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

  /** 合并队列文本落盘（目录由宿主拼，这里只交文本与格式） */
  const exportMerged = useCallback(
    async (format: OcrExportFormat) => {
      const text = mergeOcrTexts(queue, format);
      if (text.trim() === "") {
        notify("info", "队列里没有可导出的文本", "先选图识别，或等正在跑的这张跑完");
        return;
      }
      setExporting(format);
      try {
        const path = await ocrExport(text, format);
        notify("success", `合并文本已导出（${format}）`, path);
      } catch (e) {
        reportError(e, { context: "导出失败", dedupeKey: "ocr-export" });
      } finally {
        setExporting(null);
      }
    },
    [queue],
  );

  /** 清空队列 = 作废在途批次（剩下的文件不再送识别，已跑完的行随队列一起消失） */
  const clearQueue = useCallback(() => {
    runIdRef.current += 1;
    setQueue([]);
    setBusy(false);
  }, []);

  return (
    <div className={styles.root}>
      <div className={styles.toolbar}>
        <Button
          appearance="primary"
          size="small"
          disabled={busy}
          onClick={() => fileRef.current?.click()}
        >
          {busy ? "识别中…" : "选择图片识别（可多选）"}
        </Button>
        <input
          ref={fileRef}
          type="file"
          accept="image/*"
          multiple
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
        <DeferredBadge label="PDF 拆页识别" decisionRef="D-29 §9.1-⑮" />
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
        {queue.length > 0 && (
          <>
            <Text className={styles.sectionTitle}>批量队列</Text>
            <div className={styles.copyRow}>
              <Button
                size="small"
                appearance="primary"
                disabled={busy || exporting !== null}
                onClick={() => void exportMerged("txt")}
              >
                {exporting === "txt" ? "导出中…" : "导出合并 txt"}
              </Button>
              <Button
                size="small"
                disabled={busy || exporting !== null}
                onClick={() => void exportMerged("md")}
              >
                {exporting === "md" ? "导出中…" : "导出合并 md"}
              </Button>
              <Button size="small" onClick={clearQueue}>
                清空队列
              </Button>
              <span className={styles.spacer} />
              <Text className={styles.queueMeta}>
                {queue.filter((q) => q.status !== "pending").length}/{queue.length} 已完成
              </Text>
            </div>
            {queue.map((it, i) => (
              <div key={`${it.name}-${i}`} className={styles.queueRow}>
                <Text className={styles.queueName}>{it.name}</Text>
                <Badge
                  appearance="tint"
                  color={
                    it.status === "ok" ? "success" : it.status === "fail" ? "severe" : "informative"
                  }
                >
                  {queueStatusBadge(it.status)}
                </Badge>
                <Text className={styles.queueMeta}>
                  {it.status === "fail" ? (it.reason ?? "未知原因") : `${it.chars} 字`}
                </Text>
                {it.result && (
                  <Button size="small" onClick={() => void copy(it.result!.text)}>
                    复制
                  </Button>
                )}
              </div>
            ))}
          </>
        )}
        <Text className={styles.sectionTitle}>识别结果</Text>
        {busy ? (
          <EmptyState text="" loading />
        ) : err ? (
          <EmptyState text={`识别失败：${err.message}`} />
        ) : !result ? (
          <EmptyState text="尚未识别 · 点『选择图片识别（可多选）』选取 PNG/JPG（一次多张则串行逐张识别），结果可逐块复制或合并导出 txt/md" />
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
