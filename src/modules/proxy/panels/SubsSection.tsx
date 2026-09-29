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
 * 订阅子面板（T-B2-3 迁移自旧「订阅」块；流量/到期头 T-B2-10 落地）：
 * 后端仅在订阅响应携带标准头（upload/download/left/expire、profile-update-interval、
 * etag）时填充 traffic/interval_min——无头不谎显，负例由 subCard_trafficAbsent_noBadge 钉。
 * 添加成功（onAdd resolve true）才清空输入框——失败保留用户已贴的 URL 免重输。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  grow: { flex: 1, minWidth: "240px" },
});

const fmtTime = (ms: number) => (ms > 0 ? new Date(ms).toLocaleTimeString() : "未拉取");

/** 字节数人读格式（GiB 优先，保留一位小数） */
const fmtBytes = (n: number) => {
  const gib = n / 1024 / 1024 / 1024;
  if (gib >= 1) return `${gib.toFixed(1)} GB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
};

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
          {/* D-42：interval_min/etag 两枚字段随 proxy_subs 下发却不上屏——
              前者是"我手动点更新算不算太勤"的唯一依据，后者解释了为什么有时更新后节点数没变
              （服务端内容未变更走 304，不是拉取失败） */}
          {(s.interval_min !== null || s.etag !== null) && (
            <span className={styles.muted}>
              {s.interval_min !== null && `建议 ${s.interval_min} 分钟刷新`}
              {s.interval_min !== null && s.etag !== null && " · "}
              {s.etag !== null && (
                <span title={`条件请求指纹 ${s.etag}：订阅内容未变更时后端走 304，不重复拉取`}>
                  已带条件请求指纹
                </span>
              )}
            </span>
          )}
          {s.traffic && (
            <span className={styles.muted}>
              已用 {fmtBytes(s.traffic.upload + s.traffic.download)} · 剩余{" "}
              {fmtBytes(s.traffic.left)}
              {s.traffic.expire_ms > 0 &&
                ` · 到期 ${new Date(s.traffic.expire_ms).toLocaleDateString()}`}
            </span>
          )}
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
