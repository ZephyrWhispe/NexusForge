import { makeStyles, tokens } from "@fluentui/react-components";
import { SUBNAV, type ModuleId, type SubNavScope } from "./modules";

/** 二级导航（docs/DESIGN.md §3.4 + D-29 B0/T-B0-6 + T-B3-1 双维度）：
 *  分组来自全模块 SUBNAV 注册表；每个条目按自身 scope（view=子面板 / filter=筛选）
 *  与对应维度的当前选择比对高亮，两维度同屏互不覆写（session 分键，见 modules.ts 路由表） */
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
  active: Partial<Record<SubNavScope, string>>;
  onSelect: (scope: SubNavScope, id: string) => void;
  counts?: Record<string, number>;
}) {
  const styles = useStyles();
  const sections = SUBNAV[moduleId];
  return (
    <aside className={styles.root} aria-label={`${moduleId} 二级导航`}>
      {sections.map((section) => (
        <div key={section.group}>
          <span className={styles.title}>{section.group}</span>
          {section.items.map((item) => {
            const scope = item.scope ?? "filter";
            const n = item.badgeKey ? (counts?.[item.badgeKey] ?? 0) : null;
            return (
              <button
                key={`${scope}:${item.id}`}
                className={`${styles.filter} ${active[scope] === item.id ? styles.filterOn : ""}`}
                onClick={() => onSelect(scope, item.id)}
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
