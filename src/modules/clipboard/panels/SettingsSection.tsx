import SchemaForm from "../../../settings/SchemaForm";

/**
 * 统计与设置子面板（T-B3-1 骨架，细案 01§7.1 + 09 §8.1-⑪）：
 * 设置一律复用 SchemaForm（模块 config_schema 驱动，全仓唯一表单引擎，禁第二套），
 * 故剪贴板七项设置在侧栏「统计与设置」与设置中心同源同值。
 * 本批新增卡（暂停捕获、内容屏蔽规则、加密导出/导入、统计）由 T-B3-2/7/9/4 逐行填充。
 */
export default function SettingsSection() {
  return <SchemaForm moduleId="clipboard" />;
}
