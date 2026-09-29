import { Button, makeStyles, Text, tokens } from "@fluentui/react-components";
import type { ReactNode } from "react";

import { ROW_H, SPACING } from "./nfTiers";

/**
 * 设置行与设置分组（00 规范 6 节：`[标题(粗)+说明行(灰)] — spacer — [控件右端定宽]`，
 * 同页所有行控件右缘对齐同一基线＝"末端对齐"；2 节分组间 24）。
 * 规范 4-2"禁用必解释"的可机检钩子＝`disabledHint`：在场即成为控件容器的 `title`。
 * `data-nf` 钩子供门禁与真机走查命中（griffel 哈希类名不可依赖，在册教训）。
 */

const useStyles = makeStyles({
  row: {
    alignItems: "center",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    display: "flex",
    gap: SPACING.x16,
    minHeight: ROW_H.twoline,
    padding: `${SPACING.x12} 16px`,
  },
  label: { display: "flex", flex: 1, flexDirection: "column", gap: SPACING.x4, minWidth: 0 },
  description: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  control: { alignItems: "center", display: "flex", flexShrink: 0, gap: SPACING.x8 },
  group: { display: "flex", flexDirection: "column", gap: SPACING.x4 },
  groupHead: { alignItems: "center", display: "flex", gap: SPACING.x8, padding: `0 16px` },
  groupGrow: { flex: 1, minWidth: 0 },
});

export default function SwitchSetting({
  title,
  description,
  control,
  disabledHint,
}: {
  title: ReactNode;
  description?: ReactNode;
  control: ReactNode;
  disabledHint?: string;
}) {
  const styles = useStyles();
  return (
    <div className={styles.row} data-nf="setting-row">
      <div className={styles.label}>
        <Text size={300} weight="semibold" wrap={false}>
          {title}
        </Text>
        {description !== undefined && <Text className={styles.description}>{description}</Text>}
      </div>
      <div className={styles.control} data-nf="setting-control" title={disabledHint}>
        {control}
      </div>
    </div>
  );
}

export function SettingsGroup({
  title,
  onRestoreDefaults,
  children,
}: {
  title: ReactNode;
  onRestoreDefaults?: () => void;
  children: ReactNode;
}) {
  const styles = useStyles();
  return (
    <section className={styles.group} data-nf="settings-group">
      <div className={styles.groupHead}>
        <Text size={400} weight="semibold">
          {title}
        </Text>
        <div className={styles.groupGrow} />
        {onRestoreDefaults !== undefined && (
          <Button appearance="subtle" size="small" onClick={onRestoreDefaults}>
            恢复默认
          </Button>
        )}
      </div>
      <div>{children}</div>
    </section>
  );
}
