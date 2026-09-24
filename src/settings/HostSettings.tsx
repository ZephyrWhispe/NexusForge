import { makeStyles, tokens, Text } from "@fluentui/react-components";
import SchemaForm from "./SchemaForm";

/**
 * 宿主设置段（09 §7.2 T-B7-11）：托盘负载显示等「不属于任何模块」的配置。
 * 数据真源 = src-tauri tray.rs 登记的 "tray" 段（tray.json）；
 * 表单仍走 SchemaForm（全仓唯一表单引擎，禁第二套）。
 */

const useStyles = makeStyles({
  root: {
    marginTop: "10px",
    paddingTop: "6px",
    borderTop: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  title: { display: "block", paddingBottom: "4px" },
});

export default function HostSettings() {
  const styles = useStyles();
  return (
    <div className={styles.root}>
      <div className={styles.title}>
        <Text size={300} weight="semibold">
          宿主 · 托盘
        </Text>
      </div>
      <SchemaForm moduleId="tray" />
    </div>
  );
}
