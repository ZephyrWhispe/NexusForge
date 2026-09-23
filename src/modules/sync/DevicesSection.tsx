import { useState } from "react";
import {
  Badge,
  Button,
  Input,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";
import type { PairedPeerDto, SyncStatusDto } from "../../ipc/client";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 配对设备（09 §10.2 T-B5-7/T-B5-8）：一行一台已配对设备，地址由内核解析。
 *
 * - `addr_source` 是"在线/离线 + 拨号地址"两列的总闸：解析器没接线时 `online:false`
 *   的含义是"本机没查过"，渲染成"离线"就是无中生有——整列直接不出现。
 * - 手输降级为**行内高级**：默认不渲染输入框，收起即撤销该行填过的值（一个看不见却
 *   仍在生效的地址，比没有地址更难查）；留空一律折成 null，空串会被内核当成
 *   "手输了一个地址"，解析分支就此绕空。
 * - 进度数字与失败原因同一个读面（`status.peers` 现读自游标表），设备清单来自
 *   `sync_peers`（带 paired_at）：按 device_id 关联，两个来源各说各的事实，互不覆写。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  itemBody: { flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "2px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
  /** 未出账提示（T-B5-4）：黄字而非红字——落后不是故障，谎报才是 */
  warn: { color: tokens.colorPaletteDarkOrangeForeground1, fontSize: tokens.fontSizeBase200 },
});

type Props = {
  peers: PairedPeerDto[];
  status: SyncStatusDto | null;
  busy: boolean;
  loaded: boolean;
  onSync: (deviceId: string, manualAddr: string | null) => void;
  onRefresh: () => void;
};

export default function DevicesSection({ peers, status, busy, loaded, onSync, onRefresh }: Props) {
  const styles = useStyles();
  /** 行内「高级」手输的地址（按设备分桶：一台填错不该污染另一台） */
  const [manualAddr, setManualAddr] = useState<Record<string, string>>({});
  /** 哪一行的手输框展开着（null = 全收起；默认收起＝手输是例外不是常规路径） */
  const [advancedId, setAdvancedId] = useState<string | null>(null);

  /**
   * 展开/收起开关：收起时**一并撤销**该行手输的地址——藏起来却仍生效的地址
   * 比没有地址更难查，收起后就该回到"由内核解析"那个状态。
   */
  const toggleAdvanced = (deviceId: string) => {
    if (advancedId === deviceId) {
      setAdvancedId(null);
      setManualAddr((m) => {
        const next = { ...m };
        delete next[deviceId];
        return next;
      });
    } else {
      setAdvancedId(deviceId);
    }
  };

  return (
    <Section
      title={`配对设备（${peers.length}）`}
      actions={
        <>
          <Badge appearance="outline">E2E 加密 · LWW 冲突自动解</Badge>
          <Button size="small" onClick={onRefresh}>
            刷新
          </Button>
        </>
      }
    >
      {peers.length === 0 ? (
        <EmptyState
          text="暂无配对设备——先在「键鼠共享」模块完成配对（同步复用其信任根，无需二次配对）。"
          loading={!loaded}
        />
      ) : (
        peers.map((p) => {
          const prog = status?.peers.find((x) => x.device_id === p.device_id);
          return (
            <div key={p.device_id} className={styles.item}>
              <div className={styles.itemBody}>
                <div className={styles.row}>
                  <Text weight="semibold" size={300}>
                    {p.device_name}
                  </Text>
                  <Text className={styles.mono}>{p.fingerprint}</Text>
                  {status?.addr_source &&
                    (prog?.sync_addr ? (
                      <Badge appearance="tint" color="success">
                        在线
                      </Badge>
                    ) : (
                      <Badge appearance="outline" color="subtle">
                        离线
                      </Badge>
                    ))}
                </div>
                {status?.addr_source && !!prog?.sync_addr && (
                  <Text className={styles.mono}>拨号地址 {prog.sync_addr}</Text>
                )}
                <Text className={styles.muted}>
                  配对于 {new Date(p.paired_at).toLocaleString()}
                  {prog &&
                    (prog.last_sync_ms > 0
                      ? ` · 上次同步 ${new Date(prog.last_sync_ms).toLocaleString()}`
                      : " · 从未同步")}
                </Text>
                {!!prog && prog.pending_ops > 0 && (
                  <Text className={styles.warn}>未出账 {prog.pending_ops} 条</Text>
                )}
                {prog?.last_error && (
                  <InlineError text={`上次同步失败：${prog.last_error}`} />
                )}
                {advancedId === p.device_id && (
                  <div className={styles.row}>
                    <Input
                      size="small"
                      value={manualAddr[p.device_id] ?? ""}
                      onChange={(_, d) =>
                        setManualAddr((m) => ({ ...m, [p.device_id]: d.value }))
                      }
                      placeholder={prog?.sync_addr ?? "host:port"}
                      style={{ minWidth: "220px" }}
                    />
                    <Text className={styles.muted}>
                      手输仅在跨网段/发现层看不到对端时才需要；填了就以这里为准，
                      留空则仍由内核解析。
                    </Text>
                  </div>
                )}
              </div>
              <div className={styles.row}>
                <Button
                  size="small"
                  appearance="primary"
                  disabled={busy}
                  onClick={() => {
                    const manual = manualAddr[p.device_id]?.trim();
                    onSync(p.device_id, manual ? manual : null);
                  }}
                >
                  立即同步
                </Button>
                <Button
                  size="small"
                  appearance="subtle"
                  onClick={() => toggleAdvanced(p.device_id)}
                >
                  {advancedId === p.device_id ? "收起高级" : "高级"}
                </Button>
              </div>
            </div>
          );
        })
      )}
    </Section>
  );
}
