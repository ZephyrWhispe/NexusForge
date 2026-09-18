import { makeStyles, Spinner, Text, tokens } from "@fluentui/react-components";

/**
 * 空态基线（审查 D-18）：解决"假空态"——首屏数据未到达时不再直接展示
 * 引导文案冒充空列表。loading=true 渲染加载指示；否则渲染引导文案。
 * 调用方以 `loading={inFlight || !loaded}` 传入真实三态。
 */

const useStyles = makeStyles({
  root: {
    padding: "12px 8px",
    display: "flex",
    alignItems: "center",
    gap: "8px",
    justifyContent: "center",
  },
  text: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

export default function EmptyState({ text, loading = false }: { text: string; loading?: boolean }) {
  const styles = useStyles();
  if (loading) {
    return (
      <div className={styles.root} role="status">
        <Spinner size="tiny" />
        <Text className={styles.text}>加载中…</Text>
      </div>
    );
  }
  return (
    <div className={styles.root}>
      <Text className={styles.text}>{text}</Text>
    </div>
  );
}
