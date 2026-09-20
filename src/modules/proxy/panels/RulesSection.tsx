import { makeStyles, tokens, Button, Textarea } from "@fluentui/react-components";
import Section from "../../../components/Section";

/**
 * 分流子面板（T-B2-3 迁移自旧「直连域名」块——placeholder 的 \b 转义缺陷⑪a 与
 * 事件刷新覆写缺陷⑪b 按任务书归 T-B2-9 规则 v2 批同修，本行原样搬运不顺手改）。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

export default function RulesSection({
  rulesText,
  setRulesText,
  busy,
  onSave,
}: {
  rulesText: string;
  setRulesText: (v: string) => void;
  busy: string;
  onSave: () => void;
}) {
  const styles = useStyles();
  return (
    <Section
      title="直连域名"
      actions={
        <span className={styles.muted}>每行一个后缀，命中即不走代理（重新切换模式后生效）</span>
      }
    >
      <Textarea
        value={rulesText}
        onChange={(_, d) => setRulesText(d.value)}
        rows={4}
        placeholder={"cn\baidu.com\nbilibili.com"}
      />
      <div className={styles.row}>
        <Button size="small" disabled={busy !== ""} onClick={onSave}>
          保存规则
        </Button>
      </div>
    </Section>
  );
}
