/**
 * STD-06：胶囊 Tab 样式唯一出处（此前 TerminalPanel/EditorPanel 各持一份本地
 * 副本，与 components/Tabs.tsx 三处漂移）。Griffel 允许跨 makeStyles 复用
 * 静态可分析的样式对象；新 Tab 形态一律 import 本文件或 components/Tabs。
 */
import type { GriffelStyle } from "@griffel/react";
import { tokens } from "@fluentui/react-components";

export const sharedTab: GriffelStyle = {
  display: "flex",
  alignItems: "center",
  gap: "6px",
  padding: "4px 10px",
  borderRadius: tokens.borderRadiusMedium,
  border: `1px solid ${tokens.colorNeutralStroke1}`,
  cursor: "pointer",
  fontSize: tokens.fontSizeBase200,
};

export const sharedTabActive: GriffelStyle = {
  backgroundColor: tokens.colorNeutralBackground3Hover,
  border: `1px solid ${tokens.colorBrandForeground1}`,
};
