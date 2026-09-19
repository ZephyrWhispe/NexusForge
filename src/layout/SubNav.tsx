import { makeStyles, tokens } from "@fluentui/react-components";
import { SUBNAV, type ModuleId } from "./modules";

/** 二级导航（docs/DESIGN.md §3.4 + D-29 B0/T-B0-6）：分组来自全模块 SUBNAV 注册表，
 *  分组筛选（含实时计数 badgeKey）+ 后续子面板入口统一走这里 */
const useStyles = makeStyles({
  root: {
    width: "190px",
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: "12px 8px",
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground1,
  },
  title: {
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    padding: "2px 10px 10px",
    display: "block",
  },
  filter: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    width: "100%",
    height: "32px",
    padding: "0 10px",
    borderRadius: tokens.borderRadiusMedium,
    color: tokens.colorNeutralForeground2,
    backgroundColor: "transparent",
    border: "none",
    cursor: "pointer",
    fontSize: tokens.fontSizeBase300,
    ":hover": { backgroundColor: tokens.colorNeutralBackground1Hover },
  },
  filterOn: {
    backgroundColor: tokens.colorBrandBackground2,
    color: tokens.colorNeutralForeground1,
    fontWeight: tokens.fontWeightSemibold,
  },
  count: {
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
  },
});

export default function SubNav({
  moduleId,
  active,
  onSelect,
  counts,
}: {
  moduleId: ModuleId;
  active: string;
  onSelect: (id: string) => void;
  counts?: Record<string, number>;
}) {
  const styles = useStyles();
  const sections = SUBNAV[moduleId];
  return (
    <aside className={styles.root} aria-label={`${sections[0]?.group ?? ""}分组`}>
      {sections.map((section) => (
        <div key={section.group}>
          <span className={styles.title}>{section.group}</span>
          {section.items.map((item) => {
            const n = item.badgeKey ? (counts?.[item.badgeKey] ?? 0) : null;
            return (
              <button
                key={item.id}
                className={`${styles.filter} ${active === item.id ? styles.filterOn : ""}`}
                onClick={() => onSelect(item.id)}
              >
                {item.label}
                {n !== null && <span className={styles.count}>{n}</span>}
              </button>
            );
          })}
        </div>
      ))}
    </aside>
  );
}
