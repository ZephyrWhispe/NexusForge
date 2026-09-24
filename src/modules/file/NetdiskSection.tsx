import { makeStyles, Text, tokens } from "@fluentui/react-components";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 网盘档（T-B7-27，panels/04 §2「网盘」行）：整页诚实标注——网盘驱动依赖
 * sidecar 下载通道（09 §6.3-(c) 裁决定位），就绪前不立假列表、不摆假按钮，
 * 只放既有 DeferredBadge（T-B6-13 双向钉的徽标随本档从 browse 挪来，
 * 逐字未改）。
 */

const useStyles = makeStyles({
  col: { display: "flex", flexDirection: "column", gap: "8px", maxWidth: "640px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

export default function NetdiskSection() {
  const styles = useStyles();
  return (
    <div className={styles.col}>
      <Text weight="semibold" size={300}>
        网盘
      </Text>
      <Text size={200} className={styles.muted}>
        本档尚未实现：多网盘聚合依赖 sidecar 驱动通道的下载与治理就绪（裁决出处见徽标），
        就绪前整页如实标注，不提供任何半成品操作。
      </Text>
      {/* T-B6-13 明示不做（09 §6.3 双向钉）：徽标只说"没做"，不扮"禁用的就绪" */}
      <div>
        <DeferredBadge label="网盘" decisionRef="09 §6.3-(c)" />
      </div>
    </div>
  );
}
