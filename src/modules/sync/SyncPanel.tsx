import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles } from "@fluentui/react-components";
import {
  hostConfigGet,
  hostConfigSet,
  parseAppError,
  syncConflictsGet,
  syncNow,
  syncPeers,
  syncSetPaused,
  syncStatus,
  type PairedPeerDto,
  type SyncStatusDto,
} from "../../ipc/client";
import { isSyncSubPanel, useSession } from "../../stores/session";
import InlineError from "../../components/InlineError";
import ActivitySection from "./ActivitySection";
import ConflictsSection from "./ConflictsSection";
import DatasetsSection from "./DatasetsSection";
import DevicesSection from "./DevicesSection";
import OverviewSection from "./OverviewSection";
import { reportError } from "../../stores/notifications";

/**
 * 跨设备同步面板（09 §10.2 T-B5-8，细案 14-sync §2）：子面板化后的数据枢纽——
 * 五档（概览/设备/数据集/冲突/活动）按 session store 的 `syncSubPanel` 键一次只渲染
 * 一档，选择入口在 SubNav；形制照 `src/modules/proxy/ProxyPanel.tsx`，不另造分派范式。
 * 设置档不占子面板（走设置中心），这里只在概览文案里指路。
 *
 * 拓扑与真态口径（各行的设计理由写在各读面处，此处只记面板级的两条）：
 * - 数据集 v1 = 笔记库白名单（`sync_datasets_get` 读出，见 DatasetsSection）；
 *   密码库永不自动同步是内核红线，不是面板上的开关；
 * - 监听 `nf:event` 的 `sync.state_changed` / `sync.conflict`：**只作提示，不作数据源**。
 *   事件到达 = 再读一遍表（`sync_status`/`sync_peers`/`sync_conflicts_get` + 各子页
 *   经 `refreshKey` 重读）。修前该模块零事件监听、且冲突只活在一次性广播里，
 *   于是"错过事件即永久失踪"（承重⑥）。反过来把负载直接插进列表也是错的：
 *   事件丢一枚，界面就永远少一行，而那行数据其实一直在盘上。
 * - 立即同步的目的地由内核解析（T-B5-7），手输只在设备页「高级」里；
 *   两枚出账开关写完一律回读内核（T-B5-6），面板不按"我刚写了什么"下结论。
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
});

/**
 * 写完回读的预算：配置真源的 apply 是**订阅驱动的异步腿**（`run_config_feed` 在另一个
 * 任务里读盘→校验→落运行态），`host_config_set` 返回时运行态可以还没跟上。因此
 * "回读一次不符"不足以定性为被拒——给一个有预算的轮询；预算用尽仍不符才是真话：
 * 写进去了、内核没采纳（坏值在设置界面会被拒收，这里按运行态如实显示）。
 */
const REREAD_BUDGET = 5;
const REREAD_MS = 100;

/** 冲突计数的读数上限：与冲突页同一页宽，超出部分按"留存 ≥ 此数"如实说 */
const CONFLICT_PAGE = 50;

export default function SyncPanel() {
  const styles = useStyles();
  const [peers, setPeers] = useState<PairedPeerDto[]>([]);
  const [status, setStatus] = useState<SyncStatusDto | null>(null);
  const [conflictCount, setConflictCount] = useState(0);
  /** 读满一页 ⇒ 后面还有没读到的，徽标要说"N+"而不是"N"（一页长度不是总数） */
  const [conflictMore, setConflictMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [autoBusy, setAutoBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  // 首轮加载是否落定：未落定前空列表渲染加载态而非引导文案（D-18 假空态修正）
  const [loaded, setLoaded] = useState(false);
  // 事件节流计数：子页（冲突/活动）只看它"变没变"，变了就去读表，事件内容一概不进 state
  const [refreshKey, setRefreshKey] = useState(0);
  const mounted = useRef(true);

  // 子面板选择态：session store 键 syncSubPanel，旧快照缺键由 zustand 浅合并回退
  // 初始值 overview；渲染侧再经 isSyncSubPanel 收窄防野值。
  const storedSub = useSession((s) => s.syncSubPanel);
  const view = isSyncSubPanel(storedSub) ? storedSub : "overview";

  const load = useCallback(async (): Promise<SyncStatusDto | null> => {
    try {
      const [p, s, c] = await Promise.all([
        syncPeers(),
        syncStatus(),
        syncConflictsGet(CONFLICT_PAGE, 0),
      ]);
      if (!mounted.current) return null;
      setPeers(p);
      setStatus(s);
      setConflictCount(c.length);
      setConflictMore(c.length >= CONFLICT_PAGE);
      setError("");
      return s;
    } catch (e) {
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setLoaded(true);
    }
    return null;
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen("nf:event", (e) => {
          const topic = (e.payload as { topic?: string }).topic ?? "";
          if (topic === "sync.state_changed" || topic === "sync.conflict") {
            // 只当"该重读了"的门铃：数据一律从表里读回来
            setRefreshKey((k) => k + 1);
            void load();
          }
        }),
      )
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
      })
      .catch((err) =>
        reportError(err, { context: "同步面板事件监听注册失败", toast: false }),
      );
    return () => {
      cancelled = true;
      mounted.current = false;
      unlisten?.();
    };
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
  const sync = async (deviceId: string, manual: string | null) => {
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const s = await syncNow(deviceId, manual);
      setNotice(
        `同步完成：推送 ${s.pushed} · 拉取应用 ${s.pulled_applied} · 丢弃 ${s.pulled_lost} · 冲突 ${s.conflicts}`,
      );
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setBusy(false);
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
      if (!mounted.current) return;
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
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setAutoBusy(false);
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
      if (!mounted.current) return;
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
      if (mounted.current) setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      if (mounted.current) setAutoBusy(false);
    }
  };

  return (
    <div className={styles.root}>
      <InlineError text={error} />
      {!error && <InlineError text={notice} tone="success" />}

      {view === "overview" && (
        <OverviewSection
          status={status}
          conflictCount={conflictCount}
          conflictMore={conflictMore}
          autoBusy={autoBusy}
          onAutoSync={(on) => void setAutoSync(on)}
          onTogglePaused={() => void togglePaused()}
        />
      )}
      {view === "devices" && (
        <DevicesSection
          peers={peers}
          status={status}
          busy={busy}
          loaded={loaded}
          onSync={(id, manual) => void sync(id, manual)}
          onRefresh={() => void load()}
        />
      )}
      {view === "datasets" && <DatasetsSection />}
      {view === "conflicts" && <ConflictsSection refreshKey={refreshKey} />}
      {view === "activity" && <ActivitySection refreshKey={refreshKey} />}
    </div>
  );
}
