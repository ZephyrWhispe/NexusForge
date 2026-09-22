import { useCallback, useEffect, useState } from "react";
import { makeStyles, Text, tokens, Badge } from "@fluentui/react-components";
import { clipboardGroupCounts, parseAppError } from "../../../ipc/client";
import { IN_TAURI } from "../../../ipc/env";
import EmptyState from "../../../components/EmptyState";
import { GROUP_LABEL } from "../display";

/**
 * 收藏与分组子面板（T-B3-1 骨架，细案 01§2/§7.1）：左分组树读 clipboard_group_counts。
 * 重命名/删除与「建议归组」采纳流需分组写口（clipboard_entry_set_group 等），
 * 属本批 T-B3-4，故此处只读不写、也不摆假控件。
 */
const useStyles = makeStyles({
  root: { flex: 1, minWidth: 0, overflowY: "auto", padding: "14px 20px 20px" },
  title: { display: "block", fontSize: tokens.fontSizeBase400, fontWeight: tokens.fontWeightSemibold },
  hint: {
    display: "block",
    marginTop: "4px",
    marginBottom: "12px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  row: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: "12px",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusMedium,
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  name: { fontSize: tokens.fontSizeBase300 },
  count: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2 },
});

export default function GroupsSection() {
  const styles = useStyles();
  const [counts, setCounts] = useState<Record<string, number>>({});
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(() => {
    if (!IN_TAURI) {
      setLoaded(true);
      return;
    }
    clipboardGroupCounts()
      .then((c) => setCounts(c as Record<string, number>))
      .catch((e) => setError(parseAppError(e)?.data.message ?? String(e)))
      .finally(() => setLoaded(true));
  }, []);

  useEffect(refresh, [refresh]);

  const rows = Object.entries(counts).sort((a, b) => b[1] - a[1]);
  return (
    <div className={styles.root}>
      <Text className={styles.title} block>
        分组
      </Text>
      <span className={styles.hint}>
        条目按分类器词表与后续自定义分组归集（重命名、删除与建议采纳随 T-B3-4 的分组写口开放）。
      </span>
      {error && <Text className={styles.hint}>分组计数读取失败：{error}</Text>}
      {rows.length === 0 ? (
        <EmptyState text="暂无分组条目：复制内容后分类器会给出口语化分组（代码/链接/JSON/颜色/敏感）。" loading={!loaded} />
      ) : (
        rows.map(([group, n]) => (
          <div className={styles.row} key={group}>
            <span className={styles.name}>{GROUP_LABEL[group] ?? group}</span>
            <Badge appearance="outline">{n}</Badge>
          </div>
        ))
      )}
    </div>
  );
}
