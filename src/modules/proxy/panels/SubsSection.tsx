import { useState } from "react";
import {
  makeStyles,
  tokens,
  Button,
  Input,
  Text,
} from "@fluentui/react-components";
import type { ProxySubDto } from "../../../ipc/client";
import Section from "../../../components/Section";

/**
 * 订阅子面板（T-B2-3 迁移自旧「订阅」块；流量/到期头与更新策略归 T-B2-10）。
 * 添加成功（onAdd resolve true）才清空输入框——失败保留用户已贴的 URL 免重输。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  grow: { flex: 1, minWidth: "240px" },
});

const fmtTime = (ms: number) => (ms > 0 ? new Date(ms).toLocaleTimeString() : "未拉取");

export default function SubsSection({
  subs,
  busy,
  onAdd,
  onUpdate,
  onRemove,
}: {
  subs: ProxySubDto[];
  busy: string;
  onAdd: (name: string, url: string) => Promise<boolean>;
  onUpdate: (id: string) => void;
  onRemove: (sub: ProxySubDto) => void;
}) {
  const styles = useStyles();
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");

  const add = async () => {
    if (await onAdd(name, url)) {
      setName("");
      setUrl("");
    }
  };

  return (
    <Section
      title="订阅"
      actions={
        <span className={styles.muted}>自行添加分享链接或订阅地址（ss/vmess/trojan/vless/hy2/tuic/wg/ssr）</span>
      }
    >
      <div className={styles.row}>
        <Input
          className={styles.grow}
          placeholder="名称（可选）"
          value={name}
          onChange={(_, d) => setName(d.value)}
          size="small"
        />
        <Input
          className={styles.grow}
          placeholder="订阅 URL / 分享链接"
          value={url}
          onChange={(_, d) => setUrl(d.value)}
          size="small"
        />
        <Button
          size="small"
          appearance="primary"
          disabled={busy !== "" || url.trim() === ""}
          onClick={() => void add()}
        >
          添加并拉取
        </Button>
      </div>
      {subs.map((s) => (
        <div key={s.id} className={styles.row}>
          <Text size={300} weight="semibold" style={{ minWidth: "120px" }}>
            {s.name}
          </Text>
          <span className={styles.mono}>
            {s.node_count} 节点 · {fmtTime(s.updated_ms)}
          </span>
          <span className={styles.grow} />
          <Button
            size="small"
            disabled={busy !== ""}
            onClick={() => onUpdate(s.id)}
          >
            {busy === `sub-upd-${s.id}` ? "拉取中…" : "更新"}
          </Button>
          <Button size="small" disabled={busy !== ""} onClick={() => void onRemove(s)}>
            删除
          </Button>
        </div>
      ))}
    </Section>
  );
}
