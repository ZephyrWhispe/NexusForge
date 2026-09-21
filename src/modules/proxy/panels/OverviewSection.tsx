import {
  makeStyles,
  tokens,
  Badge,
  Button,
  Spinner,
  Text,
} from "@fluentui/react-components";
import type { ProxyEgressProbeDto, ProxyStatusDto } from "../../../ipc/client";
import Section from "../../../components/Section";
import DeferredBadge from "../../../components/DeferredBadge";

/**
 * 总览子面板（T-B2-3，细案 02§2 总览行）：残留恢复提示 + 模式三态大开关 +
 * 当前内核/节点/订阅概况。连通自检灯与带宽 sparkline 分属 T-B2-11/B7（此处不造占位假数据）。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  modeBtn: { minWidth: "96px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
});

const MODE_LABEL: Record<ProxyStatusDto["mode"], string> = {
  off: "关闭",
  system: "系统代理",
  tun: "TUN 模式",
};

export default function OverviewSection({
  st,
  busy,
  onMode,
  egress,
  onEgressProbe,
}: {
  st: ProxyStatusDto | null;
  busy: string;
  onMode: (mode: ProxyStatusDto["mode"]) => void;
  egress: ProxyEgressProbeDto | null;
  onEgressProbe: () => void;
}) {
  const styles = useStyles();
  return (
    <>
      {st?.restored_last_run && (
        <Section>
          <Text size={200}>
            检测到上次异常退出残留的系统代理，启动时已自动还原为你的原始设置。
          </Text>
        </Section>
      )}

      <Section
        title="运行模式"
        actions={
          <>
            {st && (
              <Badge
                appearance={st.kernel_running ? "filled" : "outline"}
                color={st.kernel_running ? "success" : "subtle"}
              >
                {st.kernel_running ? "内核运行中" : "内核已停止"}
              </Badge>
            )}
            {st?.has_backup && <Badge appearance="outline">存在原设置备份</Badge>}
            <DeferredBadge label="带宽统计" decisionRef="B7" />
          </>
        }
      >
        <div className={styles.row}>
          {(["off", "system", "tun"] as const).map((m) => (
            <Button
              key={m}
              className={styles.modeBtn}
              appearance={st?.mode === m ? "primary" : "outline"}
              disabled={busy !== "" || (m === "tun" && st != null && !st.admin)}
              onClick={() => onMode(m)}
            >
              {busy === "mode" && st?.mode !== m ? <Spinner size="tiny" /> : MODE_LABEL[m]}
            </Button>
          ))}
          {st && !st.admin && <span className={styles.muted}>TUN 需以管理员身份运行</span>}
        </div>
        {st && (
          <span className={styles.muted}>
            入站 127.0.0.1:{st.inbound_port} · 节点 {st.nodes_total} · 订阅 {st.subs_total}
            {st.mode === "tun" ? " · TUN 与系统代理互斥（已自动还原系统代理）" : ""}
          </span>
        )}
        {st && (
          <div className={styles.row}>
            <span className={styles.muted}>
              当前内核：{st.kernels.find((k) => k.id === st.kernel)?.display_name ?? st.kernel}
            </span>
            {st.kernel_id && (
              <Badge appearance="outline">
                运行：{st.kernels.find((k) => k.id === st.kernel_id)?.display_name ?? st.kernel_id}
              </Badge>
            )}
            <span className={styles.muted}>
              当前出口：
              {st.selected_node
                ? `手动 ${st.selected_node[1]}${st.selected_stale ? "（已失效：订阅更新删除了该节点，实际回落自动优选）" : ""}`
                : "自动（urltest 组自选）"}
            </span>
            <span className={styles.muted}>换核与重启在「内核」子面板操作</span>
          </div>
        )}
      </Section>

      <Section
        title="出口自检"
        actions={
          <>
            {egress === null ? (
              <Badge appearance="outline" color="subtle">
                未测
              </Badge>
            ) : egress.ok ? (
              <Badge appearance="filled" color="success">
                出口连通{egress.ms != null ? `（${egress.ms} ms）` : ""}
              </Badge>
            ) : (
              <Badge appearance="filled" color="danger">
                出口不通
              </Badge>
            )}
            <Button
              size="small"
              disabled={busy !== ""}
              onClick={onEgressProbe}
            >
              {busy === "egress" ? "自检中…" : "自检（HTTP 204）"}
            </Button>
          </>
        }
      >
        <span className={styles.muted}>
          经本地代理向 gstatic 发一次 204 探测：内核未运行/TUN 态后端如实报错（不自检假成功）。
        </span>
      </Section>
    </>
  );
}
