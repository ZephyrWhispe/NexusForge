/**
 * 完成后动作链的前端词表（09 §9.2 T-B4-8）。
 *
 * 覆盖层拿不到设置（§9.1-⑪ 不给它开 config_get 的 ACL 面），偏好由宿主经
 * `TaskStartDto.default_actions` 一次性带下来。这一处只负责两件语义小事：
 * 旧载荷缺键时退回空数组、以及把动作名单翻成钮上那句话。
 */

/** 与 Rust `POST_ACTION_WHITELIST` 同序同集（面板预设与覆盖层文案共读这一份叫法） */
export const POST_ACTION_LABELS: Record<string, string> = {
  save: "保存",
  copy: "复制",
  pin: "贴图",
  ocr: "识别",
  beautify: "美化",
};

/**
 * 「完成」钮这一次要传的动作。
 *
 * `?? []` 不是防御性噪音：升级前抓帧的旧载荷、以及 URL 参数直进覆盖层的回退路径
 * 都拿不到这一键，空数组在后端的语义正是"我没指定，按配置派生"，与 T-B4-8 之前的
 * 行为逐字一致。
 */
export function completeActions(defaultActions?: string[] | null): string[] {
  return defaultActions ?? [];
}

/** 钮上那句话：随配置真源变，"完成（保存+复制）"；空名单退回光秃秃的"完成" */
export function completeLabel(defaultActions?: string[] | null): string {
  const acts = completeActions(defaultActions);
  if (acts.length === 0) return "完成";
  const names = acts.map((a) => POST_ACTION_LABELS[a] ?? a);
  return `完成（${names.join("+")}）`;
}
