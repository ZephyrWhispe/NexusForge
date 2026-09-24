import { makeStyles, Text, tokens } from "@fluentui/react-components";
import DeferredBadge from "../../components/DeferredBadge";

/**
 * 设置档（T-B7-27，panels/04 §2「设置」行）：本批只做诚实化登记——把面板
 * 当前**已经生效的固定行为**逐条说清（预览截断限额服务端固定、删除恒走
 * 回收站、双击语义、隐藏文件带标注展示），不摆没有后端的假开关（panels/04
 * §2 列的五个开关项属后续批；无事实源就无开关）。
 */

const useStyles = makeStyles({
  col: { display: "flex", flexDirection: "column", gap: "8px", maxWidth: "640px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

export default function FileSettingsSection() {
  const styles = useStyles();
  return (
    <div className={styles.col}>
      <Text weight="semibold" size={300}>
        文件面板 · 当前固定行为
      </Text>
      <Text size={200} className={styles.muted}>
        预览：文本/图片/系统缩略图/不支持四形态；截断限额由服务端固定，UI 不提供可调上限。
      </Text>
      <Text size={200} className={styles.muted}>
        删除：恒移入系统回收站（目录连同内容整体移入），本面板不提供永久删除开关。
      </Text>
      <Text size={200} className={styles.muted}>
        双击：目录进入、文件开预览；单击为点选（累加选中集）。
      </Text>
      <Text size={200} className={styles.muted}>
        隐藏文件：随列表展示并带「(隐藏)」标注，暂无隐藏开关；列显示为固定三列（名称/大小/修改时间）。
      </Text>
      {/* T-B6-13 明示不做（09 §6.3 双向钉）：找回视图与磁盘占用图属内容深化档 */}
      <div>
        <DeferredBadge label="treemap/回收站找回" decisionRef="09 §6.3-(h)" />
      </div>
    </div>
  );
}
