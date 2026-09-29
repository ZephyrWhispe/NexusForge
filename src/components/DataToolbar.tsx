import { makeStyles, tokens } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { SHELL, SPACING } from "./nfTiers";

/**
 * 清单类工具条（00 规范 1 节高 40 随主体吸顶／5 节左右分区＋10-15 禁换行）。
 * 分区次序即判据：左区 搜索→过滤→排序，右区 批量→主按钮（主按钮恒末位＝最右）。
 */

const useStyles = makeStyles({
  root: {
    alignItems: "center",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexWrap: "nowrap",
    gap: SPACING.x8,
    minHeight: SHELL.toolbar,
    padding: `0 ${SHELL.contentPad} 0 16px`,
    position: "sticky",
    top: 0,
    zIndex: 1,
  },
  left: { alignItems: "center", display: "flex", gap: SPACING.x8, minWidth: 0 },
  grow: { flex: 1, minWidth: SPACING.x8 },
  filters: { alignItems: "center", display: "flex", gap: SPACING.x4, minWidth: 0 },
  primary: { flexShrink: 0 },
});

export default function DataToolbar({
  search,
  filters,
  sort,
  bulk,
  primary,
}: {
  search?: ReactNode;
  filters?: ReactNode;
  sort?: ReactNode;
  bulk?: ReactNode;
  primary?: ReactNode;
}) {
  const styles = useStyles();
  return (
    <div className={styles.root} role="toolbar">
      <div className={styles.left}>
        {search}
        {filters !== undefined && <div className={styles.filters}>{filters}</div>}
        {sort}
      </div>
      <div className={styles.grow} />
      {bulk}
      {primary !== undefined && <div className={styles.primary}>{primary}</div>}
    </div>
  );
}
