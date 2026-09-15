import {
  makeStyles,
  tokens,
  Input,
} from "@fluentui/react-components";
import { Search16Regular } from "@fluentui/react-icons";

/** 全局工具栏（docs/DESIGN.md §3.2：快捷入口 + 全局搜索） */
const useStyles = makeStyles({
  root: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "0 12px",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: "transparent",
  },
  btn: {
    display: "flex",
    alignItems: "center",
    gap: "7px",
    height: "30px",
    padding: "0 11px",
    borderRadius: tokens.borderRadiusMedium,
    color: tokens.colorNeutralForeground2,
    backgroundColor: "transparent",
    border: "none",
    cursor: "pointer",
    fontSize: tokens.fontSizeBase300,
    ":hover": {
      backgroundColor: tokens.colorNeutralBackground1Hover,
      color: tokens.colorNeutralForeground1,
    },
  },
  kbd: {
    fontFamily: "inherit",
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusSmall,
    padding: "1px 5px",
  },
  spacer: { flex: 1 },
  search: { width: "280px" },
});

interface Props {
  search: string;
  onSearchChange: (v: string) => void;
}

export default function Toolbar({ search, onSearchChange }: Props) {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      <button className={styles.btn}>◧ 剪切板快速面板 <span className={styles.kbd}>Ctrl+Shift+V</span></button>
      <button className={styles.btn}>⌘ 启动器 <span className={styles.kbd}>Alt+Space</span></button>
      <button className={styles.btn}>✂ 截图 <span className={styles.kbd}>Ctrl+Shift+S</span></button>
      <div className={styles.spacer} />
      <Input
        className={styles.search}
        size="small"
        contentAfter={<Search16Regular />}
        placeholder="搜索剪切板历史（FTS）…"
        value={search}
        onChange={(_, d) => onSearchChange(d.value)}
        aria-label="全局搜索"
      />
    </div>
  );
}
