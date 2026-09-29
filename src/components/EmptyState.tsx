import { makeStyles, Spinner, Text, tokens } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { SPACING } from "./nfTiers";

/**
 * 空态基线（审查 D-18）：解决"假空态"——首屏数据未到达时不再直接展示
 * 引导文案冒充空列表。loading=true 渲染加载指示；否则渲染引导文案。
 * 调用方以 `loading={inFlight || !loaded}` 传入真实三态。
 * D-42 随批补齐规范 4-4：`hint`＝为什么空，`action`＝第一步动作（空搜索≠空库两文案）。
 */

const useStyles = makeStyles({
  row: {
    alignItems: "center",
    display: "flex",
    gap: SPACING.x8,
    justifyContent: "center",
    padding: "12px 8px",
  },
  column: {
    alignItems: "center",
    display: "flex",
    flexDirection: "column",
    gap: SPACING.x8,
    justifyContent: "center",
    padding: "12px 8px",
  },
  text: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  hint: { color: tokens.colorNeutralForeground4, fontSize: tokens.fontSizeBase200 },
});

export default function EmptyState({
  text,
  loading = false,
  hint,
  action,
}: {
  text: string;
  loading?: boolean;
  hint?: ReactNode;
  action?: ReactNode;
}) {
  const styles = useStyles();
  if (loading) {
    return (
      <div className={styles.row} role="status">
        <Spinner size="tiny" />
        <Text className={styles.text}>加载中…</Text>
      </div>
    );
  }
  if (hint === undefined && action === undefined) {
    // 旧调用形态逐字保留（30+ 站点零改动、零回归面）
    return (
      <div className={styles.row}>
        <Text className={styles.text}>{text}</Text>
      </div>
    );
  }
  return (
    <div className={styles.column}>
      <Text className={styles.text}>{text}</Text>
      {hint !== undefined && <Text className={styles.hint}>{hint}</Text>}
      {action}
    </div>
  );
}
