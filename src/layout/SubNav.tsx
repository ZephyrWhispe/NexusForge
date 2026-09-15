import { makeStyles, tokens } from "@fluentui/react-components";
import { CLIP_GROUPS } from "./modules";

/** 二级导航（docs/DESIGN.md §3.4；剪切板模块：分组筛选 + 视图开关） */
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
  sectionTitle: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    padding: "14px 10px 6px",
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

export default function SubNav({ active, onSelect }: { active: string; onSelect: (id: string) => void }) {
  const styles = useStyles();
  return (
    <aside className={styles.root} aria-label="剪切板分组">
      <span className={styles.title}>剪切板</span>
      {CLIP_GROUPS.map((g) => (
        <button
          key={g.id}
          className={`${styles.filter} ${active === g.id ? styles.filterOn : ""}`}
          onClick={() => onSelect(g.id)}
        >
          {g.name}
          <span className={styles.count}>{g.count}</span>
        </button>
      ))}
      <span className={styles.sectionTitle}>视图</span>
      <button className={styles.filter}>仅置顶</button>
      <button className={styles.filter}>标签管理</button>
      <button className={styles.filter}>永不记录黑名单</button>
    </aside>
  );
}
