import { useCallback, useEffect, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Spinner,
  Switch,
} from "@fluentui/react-components";
import {
  hostConfigGet,
  hostConfigSet,
  parseAppError,
  syncNow,
  syncPeers,
  syncSetPaused,
  syncStatus,
  type PairedPeerDto,
  type SyncStatusDto,
} from "../../ipc/client";
import Section from "../../components/Section";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";
import ActivitySection from "./ActivitySection";
import ConflictsSection from "./ConflictsSection";

/**
 * 跨设备同步面板（docs/impl/07 SYNC1–SYNC4，M15 v1）：
 * - 拓扑：局域网 P2P（信任根复用 KVM 配对；端到端加密）
 * - 数据集 v1 = 笔记库；密码库永不自动同步
 * - 冲突：LWW 自动解 + sync.conflict 事件通知
 * - 监听真态来自 `status.listening`（T-B5-4）：**不是**端口号在场的同义词。修前
 *   `start()` 无条件置 Running、面板直读 `status.port` ⇒ 端口被占时照样显示"监听 :49820"。
 * - 地址不再手输（T-B5-7）：对端 sync 端口由心跳宣告，内核按"发现层 → 最近一次成功地址"
 *   解析；面板每行显示在线态与拨号地址，手输降级为行内「高级」。
 *   但 `addr_source=false`（本机没接发现层解析器）时这两列**整列不渲染**——那时
 *   `online:false` 的含义是"没查过"，把它显示成"离线"就是撒谎（与 T-B5-4 同一纪律）。
 * - 两枚出账开关（T-B5-6）：`auto_sync` 是配置（走 `host_config_*` 真源，静默窗到期自动
 *   出账），`paused` 是运行态位（走 `sync_set_paused`）。两者都**写完回读内核**再显示，
 *   面板不按"我刚写了什么"下结论——被拒的配置值进得了盘也进不了运行态。
 * 面板内无删除/解绑类操作（解除配对只在「键鼠共享」面板做，D-18 已在那里加确认）。
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

/**
 * 写完回读的预算：配置真源的 apply 是**订阅驱动的异步腿**（`run_config_feed` 在另一个
 * 任务里读盘→校验→落运行态），`host_config_set` 返回时运行态可以还没跟上。因此
 * "回读一次不符"不足以定性为被拒——给一个有预算的轮询；预算用尽仍不符才是真话：
 * 写进去了、内核没采纳（坏值在设置界面会被拒收，这里按运行态如实显示）。
 */
const REREAD_BUDGET = 5;
const REREAD_MS = 100;

export default function SyncPanel() {
  const styles = useStyles();
  const [peers, setPeers] = useState<PairedPeerDto[]>([]);
  const [status, setStatus] = useState<SyncStatusDto | null>(null);
  /** 行内「高级」手输的地址（按设备分桶：一台填错不该污染另一台） */
  const [manualAddr, setManualAddr] = useState<Record<string, string>>({});
  /** 哪一行的手输框展开着（null = 全收起；默认收起＝手输是例外不是常规路径） */
  const [advancedId, setAdvancedId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [autoBusy, setAutoBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  // 首轮加载是否落定：未落定前空列表渲染加载态而非引导文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);

  const load = useCallback(async (): Promise<SyncStatusDto | null> => {
    try {
      const [p, s] = await Promise.all([syncPeers(), syncStatus()]);
      setPeers(p);
      setStatus(s);
      setError("");
      return s;
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setLoaded(true);
    }
    return null;
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 回读到 `settled` 或预算用尽，返回最后一次读数（null = 读本身失败，错误已在 load 里落）。 */
  const rereadStatus = async (
    settled: (s: SyncStatusDto) => boolean,
  ): Promise<SyncStatusDto | null> => {
    let s = await load();
    for (let i = 0; i < REREAD_BUDGET && s && !settled(s); i++) {
      await new Promise((r) => setTimeout(r, REREAD_MS));
      s = await load();
    }
    return s;
  };

  /**
   * 立即同步（T-B5-7）：地址交给内核解析，面板只在用户**显式**填了「高级」时才带地址。
   *
   * 空串一律折成 `null`——把空串传下去会被当成"手输了一个地址"，解析分支就此绕空。
   * 成功文案只报这次会话的账，不报"发到了哪个地址"：`SyncSummary` 里没有这一项，
   * 面板要显示就得自己再解析一遍，那就是把内核的决定在前端重做（两处会漂移）。
   */
  const sync = async (deviceId: string) => {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const manual = manualAddr[deviceId]?.trim();
      const s = await syncNow(deviceId, manual ? manual : null);
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

  /**
   * 行内「高级」开关：收起时**一并撤销**该行手输的地址。
   * 一个看不见却仍在生效的地址比没有地址更难查——收起后就该回到"由内核解析"那个状态。
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

  /**
   * 自动同步总开关（配置真源）。两句纪律：
   * - **读-改-写**：`host_config_set` 是整份替换语义，只带 `auto_sync` 一键会把用户
   *   调过的静默窗/冲突保留窗一起抹掉，所以先 `hostConfigGet` 再展开覆盖；
   * - 结论只从**回读到的运行态**下：`status.auto_sync` 是内核内存态，不是盘上的值，
   *   两者不一致就是"写进去了但没采纳"，得说没采纳。
   */
  const setAutoSync = async (on: boolean) => {
    setAutoBusy(true);
    setError("");
    setNotice("");
    try {
      const cfg = await hostConfigGet("sync");
      await hostConfigSet("sync", { ...cfg, auto_sync: on });
      const s = await rereadStatus((x) => x.auto_sync === on);
      if (s?.auto_sync === on) {
        setNotice(
          on
            ? "自动同步已开启：本地变更入流后按静默窗自动出账（只发给本机成功同步过的地址）"
            : "自动同步已关闭：变更照常入流，出账只在点「立即同步」时发生",
        );
      } else {
        setError(
          `配置已写入但内核未采纳（当前运行态：自动同步 ${s?.auto_sync ? "开" : "关"}）` +
            "——面板按运行态显示，不按这次点击下结论",
        );
      }
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setAutoBusy(false);
    }
  };

  /**
   * 暂停/恢复（运行态位，非配置）：只掐自动出账的静默窗，手动「立即同步」这条腿照常可用；
   * 恢复**不补跑**（要立刻出账那里有「立即同步」）。同样写完回读。
   */
  const togglePaused = async () => {
    const next = !(status?.paused ?? false);
    setAutoBusy(true);
    setError("");
    setNotice("");
    try {
      await syncSetPaused(next);
      const s = await rereadStatus((x) => x.paused === next);
      if (s?.paused === next) {
        setNotice(
          next
            ? "已暂停自动出账：入流照常累积，恢复后不补跑（要立刻出账点「立即同步」）"
            : "已恢复自动出账（总开关开着才会真的排程）",
        );
      } else {
        setError(
          `暂停位未生效（当前运行态：${s?.paused ? "已暂停" : "未暂停"}）——手动同步不受影响`,
        );
      }
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setAutoBusy(false);
    }
  };

  return (
    <div className={styles.root}>
      <InlineError text={error} />
      {!error && <InlineError text={notice} tone="success" />}

      <Section
        title="同步状态"
        actions={
          <>
            {status && (
              <>
                <Badge appearance="outline">变更记录 {status.op_count} 条</Badge>
                <Badge
                  appearance="tint"
                  color={status.listening ? "success" : "danger"}
                >
                  {status.listening ? `监听 :${status.port}` : `未监听 :${status.port}`}
                </Badge>
                {/* 暂停位是运行态真值（T-B5-6）：只说"暂停/未暂停"，不据 auto_sync 推断——
                    总开关关着时暂停位同样是 false，那是两个独立事实。 */}
                <Badge
                  appearance="tint"
                  color={status.paused ? "warning" : "brand"}
                >
                  {status.paused ? "已暂停自动出账" : "未暂停自动出账"}
                </Badge>
              </>
            )}
            {(busy || autoBusy) && <Spinner size="tiny" />}
          </>
        }
      >
        <InlineError
          text={
            status && !status.listening
              ? status.last_bind_error ??
                `监听未就绪（端口 ${status.port}）——本机仍可主动发起同步，但对端连不进来`
              : ""
          }
        />
        <div className={styles.row}>
          <Switch
            label="自动同步"
            checked={status?.auto_sync ?? false}
            disabled={!status || autoBusy}
            onChange={(_, d) => void setAutoSync(d.checked)}
          />
          <Text className={styles.muted}>
            开启后：本地变更入流即排静默窗，窗口内的连续编辑合并成一轮自动出账。
          </Text>
          <Button size="small" disabled={!status || autoBusy} onClick={() => void togglePaused()}>
            {status?.paused ? "恢复自动出账" : "暂停自动出账"}
          </Button>
        </div>
        <Text className={styles.muted}>
          对端地址由局域网发现层给出（心跳宣告各自的同步端口），无需手输；两端须已通过
          「键鼠共享」配对。个别设备不在同一网段时，在该设备行展开「高级」手输 host:port。
        </Text>
      </Section>

      <Section
        title={`配对设备（${peers.length}）`}
        actions={
          <>
            <Badge appearance="outline">E2E 加密 · LWW 冲突自动解</Badge>
            <Button size="small" onClick={() => void load()}>
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
            // 进度来自 status.peers（现读自三张表），设备清单来自 sync_peers（带 paired_at）：
            // 按 device_id 关联，两个来源各说各的事实，互不覆写。
            const prog = status?.peers.find((x) => x.device_id === p.device_id);
            return (
              <div key={p.device_id} className={styles.item}>
                <div className={styles.itemBody}>
                  <div className={styles.row}>
                    <Text weight="semibold" size={300}>
                      {p.device_name}
                    </Text>
                    <Text className={styles.mono}>{p.fingerprint}</Text>
                    {/* `addr_source` 是这一列的总闸：解析器没接线时 online=false 的意思是
                        "本机没查过"，渲染成"离线"就是无中生有——整列直接不出现。 */}
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
                    onClick={() => void sync(p.device_id)}
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

      <ActivitySection />

      <ConflictsSection />

      <Section title="同步范围">
        <Text className={styles.muted}>
          v1 数据集：笔记库（新建/修改/删除实时入变更流）。密码库条目
          <Text weight="semibold">永不</Text>
          自动同步（仅手动导出加密包）。
          冲突策略：同一笔记双向修改按时间戳取最新（LWW），被覆盖一侧以 sync.conflict 事件提示。
        </Text>
      </Section>
    </div>
  );
}
