import { makeStyles, Text, tokens } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { SHELL, SPACING } from "./nfTiers";

/**
 * 清单页脚骨架（00 规范 1 节：Footer 32，选中 n 项·共大小·分页；5 节"n / 共 m"）。
 * 不 sticky——规范只把工具条列为吸顶件，页脚随主体滚动。
 * `data-nf="list-footer"`＝真机走查的取件钩子（griffel 哈希类不可依赖）。
 */

const useStyles = makeStyles({
  root: {
    alignItems: "center",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    boxSizing: "border-box",
    color: tokens.colorNeutralForeground3,
    display: "flex",
    gap: SPACING.x12,
    minHeight: SHELL.footer,
    padding: `0 ${SHELL.contentPad} 0 16px`,
  },
  grow: { flex: 1, minWidth: 0 },
});

export default function ListFooter({ left, right }: { left?: ReactNode; right?: ReactNode }) {
  const styles = useStyles();
  return (
    <div className={styles.root} data-nf="list-footer">
      <Text size={200}>{left}</Text>
      <div className={styles.grow} />
      <Text size={200}>{right}</Text>
    </div>
  );
}
