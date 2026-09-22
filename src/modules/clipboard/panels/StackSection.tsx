import { makeStyles, Text, tokens } from "@fluentui/react-components";
import EmptyState from "../../../components/EmptyState";

/**
 * 粘贴堆栈子面板（T-B3-1 骨架，细案 01§2/§7.1）：队列编辑器视图（序号/摘要/重排/逐条弹出/全部粘贴）。
 * 后端 push_stack/pop_stack 与 paste_stack 表俱在但零命令注册（09 §8.1-②），
 * 堆栈命令面与 Ctrl+V 投递在 T-B3-3 落地——挂载期本面板不发任何 invoke（诚实空态，非假列表）。
 */
const useStyles = makeStyles({
  root: { flex: 1, minWidth: 0, overflowY: "auto", padding: "14px 20px 20px" },
  title: { display: "block", fontSize: tokens.fontSizeBase400, fontWeight: tokens.fontWeightSemibold },
  hint: {
    display: "block",
    marginTop: "4px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
});

export default function StackSection() {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      <Text className={styles.title} block>
        粘贴堆栈
      </Text>
      <span className={styles.hint}>
        把多条内容排进队列，按入栈顺序逐条粘贴到目标应用（或一次性全部粘贴、带间隔）。
      </span>
      <EmptyState text="堆栈命令面（入栈/队列/逐条投递）随本批 T-B3-3 开通后，本页显示队列与粘贴操作。" />
    </div>
  );
}
