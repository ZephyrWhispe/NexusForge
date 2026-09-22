import { makeStyles, Text, Button, tokens } from "@fluentui/react-components";
import {
  SCROLL_DISCARD_LABEL,
  SCROLL_FINISH_LABEL,
  scrollStripCopy,
  type ScrollState,
} from "./scrollFlow";

const useStyles = makeStyles({
  strip: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "6px 10px",
    borderTop: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: "rgba(0,0,0,.72)",
  },
  copy: { color: "#fff", flex: 1, minWidth: 0 },
});

/**
 * 滚动截图步进条（T-B4-5 装配半，贴在覆盖层工具条下方）。
 *
 * 只有两钮：**[完成拼接] / [放弃]**。"采下一帧"那一下在工具条的「滚动」钮上——那颗钮
 * 本来就是开一次会话的入口，会话开着时它的文案变成「已滚一段，点此追加」——因此步进条
 * 里没有第三种动作：整条通路不代用户滚动一下。
 */
export default function ScrollStrip({
  state,
  onFinish,
  onDiscard,
}: {
  state: ScrollState;
  onFinish: () => void;
  onDiscard: () => void;
}) {
  const styles = useStyles();
  return (
    <div className={styles.strip} data-scroll-strip="">
      <Text size={200} className={styles.copy}>
        {scrollStripCopy(state)}
      </Text>
      <Button size="small" appearance="primary" disabled={state.busy} onClick={onFinish}>
        {SCROLL_FINISH_LABEL}
      </Button>
      <Button size="small" appearance="subtle" disabled={state.busy} onClick={onDiscard}>
        {SCROLL_DISCARD_LABEL}
      </Button>
    </div>
  );
}
