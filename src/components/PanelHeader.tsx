import { makeStyles, Text, tokens } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { ROW_H, SHELL, SPACING } from "./nfTiers";

/**
 * 面板头部骨架（00 规范 1 节：标题(左)│上下文摘要│主操作区(右)，高 48、固定不滚动）。
 * `actions` 恒为行内最后一个子节点＝"主操作恒最右"（3 节）由此可机检。
 */

const useStyles = makeStyles({
  root: {
    alignItems: "center",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    display: "flex",
    gap: SPACING.x12,
    minHeight: ROW_H.base,
    padding: `0 ${SHELL.contentPad} 0 ${SPACING.x16}`,
  },
  title: { flexShrink: 0 },
  grow: { flex: 1, minWidth: 0 },
  context: {
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
    minWidth: 0,
    overflowX: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
});

export default function PanelHeader({
  title,
  context,
  actions,
}: {
  title: ReactNode;
  context?: ReactNode;
  actions?: ReactNode;
}) {
  const styles = useStyles();
  return (
    <header className={styles.root}>
      <Text size={500} weight="semibold" className={styles.title}>
        {title}
      </Text>
      {context !== undefined && (
        <Text className={styles.context} title={typeof context === "string" ? context : undefined}>
          {context}
        </Text>
      )}
      <div className={styles.grow} />
      {actions}
    </header>
  );
}
