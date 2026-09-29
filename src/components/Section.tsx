import { makeStyles, tokens, Text } from "@fluentui/react-components";
import type { ReactNode } from "react";

/**
 * 卡片区块基线（审查 D-18）：`.section` 卡片此前在 9 个面板各写一遍，
 * 统一到此组件。head 行 = 标题 + 弹性空隙 + 操作区（原 sectionHead 4 份重复）。
 */

const useStyles = makeStyles({
  root: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "16px",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexDirection: "column",
    gap: "12px",
  },
  head: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  grow: { flex: 1, minWidth: "0" },
});

export default function Section({
  title,
  actions,
  children,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      {(title !== undefined || actions !== undefined) && (
        <div className={styles.head}>
          {title !== undefined && (
            <Text size={300} weight="semibold">
              {title}
            </Text>
          )}
          <div className={styles.grow} />
          {actions}
        </div>
      )}
      {children}
    </div>
  );
}
