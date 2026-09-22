import { makeStyles, Text, tokens } from "@fluentui/react-components";
import EmptyState from "../../../components/EmptyState";

/**
 * 敏感库子面板（T-B3-1 骨架，细案 01§2 + §7.1 掩码行不跳版）：
 * 现状是"密文入库 + 列表视觉掩码"，但通用读口 clipboard_get 对敏感行仍直返明文
 * （09 §8.1-⑤ 读口比写口宽），故"按需揭示"不是纯 UI 活：显式揭示口 + 收口读门在 T-B3-5。
 * 本页在揭示口就绪前不发任何 invoke——宁可空态，也不拿一个可绕过的假安全视图糊弄用户。
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

export default function SecretSection() {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      <Text className={styles.title} block>
        敏感库
      </Text>
      <span className={styles.hint}>
        检测到密钥/Token 的条目以 AES-256-GCM 信封入库（D-04），列表恒显掩码。本页提供掩码清单与需二次确认的揭示入口。
      </span>
      <EmptyState text="揭示门（显式揭示口 + 通用读口收紧）随本批 T-B3-5 开通；在此之前可用「筛选 → 敏感」查看掩码清单。" />
    </div>
  );
}
