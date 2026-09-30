import { makeStyles, tokens, Text } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { ROW_H } from "./nfTiers";

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
  // 锚点落点留出壳层工具条行高（44＝ROW_H.compact，四的倍数档）
  anchored: { scrollMarginTop: ROW_H.compact },
});

export default function Section({
  title,
  actions,
  anchor,
  children,
}: {
  title?: ReactNode;
  actions?: ReactNode;
  /** D-43 锚点式二级导航的落点标记（data-nf-sec）；44px＝壳层工具条行高，滚动定位留白 */
  anchor?: string;
  children?: ReactNode;
}) {
  const styles = useStyles();
  return (
    <div
      className={anchor === undefined ? styles.root : `${styles.root} ${styles.anchored}`}
      data-nf="sec"
      data-nf-sec={anchor}
    >
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
