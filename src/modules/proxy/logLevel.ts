/**
 * 内核日志级别判定（T-B2-3，09 §5.2 字面：行前缀 info/warning/error/debug/trace 判定）。
 * 任务书签钉的形态是 `[info] …`；同时兼容已知真实输出格式——
 * `level=info`（logrus key=value）、`… INFO …`（大写时间戳式）、`info: …`（sing-box CLI 式）。
 * 首个级别词即结论；无任何级别词的返回 null（只出现在「全部」，各级别芯片均滤除，不猜级别）。
 */
export type LogLevel = "info" | "warning" | "error" | "debug" | "trace";

export const LOG_LEVELS: readonly LogLevel[] = ["info", "warning", "error", "debug", "trace"];

const LEVEL_RE = /(?:^|[\s=[:[])(info|warning|warn|error|debug|trace)(?=$|[\s:=\]])/i;

export function logLevelOf(text: string): LogLevel | null {
  const m = LEVEL_RE.exec(text);
  if (!m) return null;
  const lv = m[1].toLowerCase();
  return lv === "warn" ? "warning" : (lv as LogLevel);
}
