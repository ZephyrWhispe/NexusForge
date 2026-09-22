/** 剪切板展示层共用件（T-B3-1 拆分时自 ClipboardPanel 提出，五子面板同源复用） */

/** 固定词表分组中文名；未收录者（用户自定义分组，T-B3-4）原样显示组名 */
export const GROUP_LABEL: Record<string, string> = {
  url: "链接",
  json: "JSON",
  code: "代码",
  color: "颜色",
  secret: "敏感",
};

export function fmtTime(ts: number): string {
  const d = new Date(ts);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}
