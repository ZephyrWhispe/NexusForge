import { useCallback, useEffect, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Spinner,
} from "@fluentui/react-components";
import {
  parseAppError,
  syncNow,
  syncPeers,
  syncStatus,
  type PairedPeerDto,
  type SyncStatusDto,
} from "../../ipc/client";

/**
 * 跨设备同步面板（docs/impl/07 SYNC1–SYNC4，M15 v1）：
 * - 拓扑：局域网 P2P（信任根复用 KVM 配对；端到端加密）
 * - 数据集 v1 = 笔记库；密码库永不自动同步
 * - 冲突：LWW 自动解 + sync.conflict 事件通知
 */
const useStyles = makeStyles({
  root: {
    flex: 1,
    minWidth: 0,
    overflowY: "auto",
    padding: "0 20px 20px",
    display: "flex",
    flexDirection: "column",
    gap: "16px",
  },
  section: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    padding: "12px 16px",
    backgroundColor: tokens.colorNeutralBackground1,
    display: "flex",
    flexDirection: "column",
    gap: "10px",
  },
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
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  ok: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
});

export default function SyncPanel() {
  const styles = useStyles();
  const [peers, setPeers] = useState<PairedPeerDto[]>([]);
  const [status, setStatus] = useState<SyncStatusDto | null>(null);
  const [addr, setAddr] = useState("127.0.0.1:49820");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const load = useCallback(async () => {
    try {
      const [p, s] = await Promise.all([syncPeers(), syncStatus()]);
      setPeers(p);
      setStatus(s);
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const sync = async (deviceId: string) => {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const s = await syncNow(deviceId, addr.trim());
      setNotice(
        `同步完成：推送 ${s.pushed} · 拉取应用 ${s.pulled_applied} · 丢弃 ${s.pulled_lost} · 冲突 ${s.conflicts}`,
      );
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.root}>
      {error && <Text className={styles.error}>{error}</Text>}
      {!error && notice && <Text className={styles.ok}>{notice}</Text>}

      <div className={styles.section}>
        <div className={styles.row}>
          <Text weight="semibold">同步状态</Text>
          {status && (
            <>
              <Badge appearance="outline">变更记录 {status.op_count} 条</Badge>
              <Badge appearance="outline">监听 :{status.port}</Badge>
            </>
          )}
          {busy && <Spinner size="tiny" />}
        </div>
        <div className={styles.row}>
          <Input
            size="small"
            value={addr}
            onChange={(_, d) => setAddr(d.value)}
            placeholder="对端地址 host:port"
            style={{ minWidth: "220px" }}
          />
          <Text className={styles.muted}>对端地址（局域网 IP + 端口）；两端须已通过「键鼠共享」配对。</Text>
        </div>
      </div>

      <div className={styles.section}>
        <div className={styles.row}>
          <Text weight="semibold">配对设备（{peers.length}）</Text>
          <Badge appearance="outline">E2E 加密 · LWW 冲突自动解</Badge>
          <div style={{ flex: 1 }} />
          <Button size="small" onClick={() => void load()}>
            刷新
          </Button>
        </div>
        {peers.length === 0 ? (
          <Text className={styles.muted}>
            暂无配对设备——先在「键鼠共享」模块完成配对（同步复用其信任根，无需二次配对）。
          </Text>
        ) : (
          peers.map((p) => (
            <div key={p.device_id} className={styles.item}>
              <div className={styles.itemBody}>
                <div className={styles.row}>
                  <Text weight="semibold" size={300}>
                    {p.device_name}
                  </Text>
                  <Text className={styles.mono}>{p.fingerprint}</Text>
                </div>
                <Text className={styles.muted}>配对于 {new Date(p.paired_at).toLocaleString()}</Text>
              </div>
              <Button
                size="small"
                appearance="primary"
                disabled={busy}
                onClick={() => void sync(p.device_id)}
              >
                立即同步
              </Button>
            </div>
          ))
        )}
      </div>

      <div className={styles.section}>
        <Text weight="semibold">同步范围</Text>
        <Text className={styles.muted}>
          v1 数据集：笔记库（新建/修改/删除实时入变更流）。密码库条目**永不**自动同步（仅手动导出加密包）。
          冲突策略：同一笔记双向修改按时间戳取最新（LWW），被覆盖一侧以 sync.conflict 事件提示。
        </Text>
      </div>
    </div>
  );
}
