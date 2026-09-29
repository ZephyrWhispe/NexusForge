import { makeStyles, tokens } from "@fluentui/react-components";
import { SUBNAV, type ModuleId, type SubNavScope } from "./modules";

/** 二级导航（docs/DESIGN.md §3.4 + D-29 B0/T-B0-6 + T-B3-1 双维度）：
 *  分组来自全模块 SUBNAV 注册表；每个条目按自身 scope（view=子面板 / filter=筛选）
 *  与对应维度的当前选择比对高亮，两维度同屏互不覆写（session 分键，见 modules.ts 路由表）
 *
 *  D-42 窄栏形态经 CSS 自定义属性下发（global.css 的 @container 只在容器上改三个变量）：
 *  griffel 运行时注入的类与 global.css 同特异度且次序在后，直接写 width/display 会静默失效
 *  （真机实测：206.8px 三档不变）。变量＋字面量兜底＝唯一不依赖层叠次序的通路。 */
const useStyles = makeStyles({
  root: {
    display: "var(--nf-subnav-display, block)",
    width: "var(--nf-subnav-w, 190px)",
    boxSizing: "border-box",
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
    padding: `12px var(--nf-subnav-pad, 8px)`,
    overflowY: "auto",
    backgroundColor: tokens.colorNeutralBackground1,
  },
  title: {
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    padding: "2px 10px 10px",
    display: "var(--nf-subnav-group-display, block)",
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
    <aside className={styles.root} data-nf="subnav" aria-label={`${moduleId} 二级导航`}>
      {sections.map((section) => (
        <div key={section.group}>
          <span className={styles.title} data-nf="subnav-group">
            {section.group}
          </span>
          {section.items.map((item) => {
            const scope = item.scope ?? "filter";
            const n = item.badgeKey ? (counts?.[item.badgeKey] ?? 0) : null;
            return (
              <button
                key={`${scope}:${item.id}`}
                className={`${styles.filter} ${active[scope] === item.id ? styles.filterOn : ""}`}
                title={item.label}
                onClick={() => onSelect(scope, item.id)}
              >
                <span data-nf="subnav-label">{item.label}</span>
                {n !== null && <span className={styles.count}>{n}</span>}
              </button>
            );
          })}
        </div>
      ))}
    </aside>
  );
}
