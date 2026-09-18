import { makeStyles, tokens } from "@fluentui/react-components";

/**
 * 胶囊选项卡基线（审查 D-18）：tab 样式此前 5 份重复，且多数实现是
 * 裸 `<button>` 无 role="tab"/aria-selected。共享组件内部渲染真实
 * button + 字面量 ARIA 属性（jsx-a11y 对 spread 不可见，故不写成属性工厂）。
 */

const useStyles = makeStyles({
  list: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  tab: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "4px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    cursor: "pointer",
    fontSize: tokens.fontSizeBase200,
  },
  tabActive: {
    backgroundColor: tokens.colorNeutralBackground3Hover,
    border: `1px solid ${tokens.colorBrandForeground1}`,
  },
});

export interface TabItem<T extends string> {
  id: T;
  label: string;
}

export default function Tabs<T extends string>({
  items,
  value,
  onChange,
  ariaLabel,
}: {
  items: TabItem<T>[];
  value: T;
  onChange: (id: T) => void;
  ariaLabel: string;
}) {
  const styles = useStyles();
  return (
    <div className={styles.list} role="tablist" aria-label={ariaLabel}>
      {items.map((it) => (
        <button
          key={it.id}
          type="button"
          role="tab"
          aria-selected={value === it.id}
          className={`${styles.tab} ${value === it.id ? styles.tabActive : ""}`}
          onClick={() => onChange(it.id)}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}
