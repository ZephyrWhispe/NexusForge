import {
  Badge,
  Button,
  makeStyles,
  Spinner,
  Switch,
  Text,
  tokens,
} from "@fluentui/react-components";
import type { SyncStatusDto } from "../../ipc/client";
import Section from "../../components/Section";
import DeferredBadge from "../../components/DeferredBadge";
import InlineError from "../../components/InlineError";

/**
 * 同步概览（09 §10.2 T-B5-8）：出账开关与监听真态在同一张卡上。
 *
 * 两处纪律随读面 inherited（设计理由见各行落点，此处只钉口径）：
 * - 监听徽标跟 `listening` 而非"端口号在场"（T-B5-4：把"启动了"说成"在听"是本模块最显眼的假绿）；
 * - 开关结论只从**写完回读到的运行态**下（T-B5-6：被拒的配置值进得了盘也进不了运行态）。
 * 冲突计数徽标读的是 `sync_conflicts_get` 的表行数（T-B5-8 红线：事件只作提示、不作数据源），
 * 与「冲突」页顶部计数同源——两处不一致说明有一处在骗人，而不是"丢了事件"。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

type Props = {
  status: SyncStatusDto | null;
  conflictCount: number;
  /** 读满一页：徽标说"N+"，一页长度不等于总数 */
  conflictMore: boolean;
  autoBusy: boolean;
  onAutoSync: (on: boolean) => void;
  onTogglePaused: () => void;
};

export default function OverviewSection({
  status,
  conflictCount,
  conflictMore,
  autoBusy,
  onAutoSync,
  onTogglePaused,
}: Props) {
  const styles = useStyles();
  return (
    <Section
      title="同步状态"
      actions={
        <>
          {status && (
            <>
              <Badge appearance="outline">变更记录 {status.op_count} 条</Badge>
              <Badge appearance="tint" color={status.listening ? "success" : "danger"}>
                {status.listening ? `监听 :${status.port}` : `未监听 :${status.port}`}
              </Badge>
              {/* 暂停位是运行态真值（T-B5-6）：只说"暂停/未暂停"，不据 auto_sync 推断——
                  总开关关着时暂停位同样是 false，那是两个独立事实。 */}
              <Badge appearance="tint" color={status.paused ? "warning" : "brand"}>
                {status.paused ? "已暂停自动出账" : "未暂停自动出账"}
              </Badge>
              <Badge appearance="outline">
                冲突留存 {conflictCount}
                {conflictMore ? "+" : ""} 条
              </Badge>
            </>
          )}
          {autoBusy && <Spinner size="tiny" />}
          <DeferredBadge label="云中转" decisionRef="09 §10.2-9(b)" />
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
          onChange={(_, d) => onAutoSync(d.checked)}
        />
        <Text className={styles.muted}>
          开启后：本地变更入流即排静默窗，窗口内的连续编辑合并成一轮自动出账。
        </Text>
        <Button size="small" disabled={!status || autoBusy} onClick={onTogglePaused}>
          {status?.paused ? "恢复自动出账" : "暂停自动出账"}
        </Button>
      </div>
      <Text className={styles.muted}>
        对端地址由局域网发现层给出（心跳宣告各自的同步端口），无需手输；两端须已通过
        「键鼠共享」配对。个别设备不在同一网段时，在「设备」页该设备行展开「高级」手输
        host:port。静默窗与冲突保留天数在「设置中心 → 跨设备同步」里调。
      </Text>
    </Section>
  );
}
