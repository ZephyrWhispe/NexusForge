import { useState } from "react";
import {
  makeStyles,
  tokens,
  Badge,
  Button,
  Input,
  Text,
} from "@fluentui/react-components";
import type { ProxyKernelInfoDto, ProxyNodeDto, ProxyStatusDto } from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import Section from "../../../components/Section";

/**
 * 内核子面板（T-B2-3，09 §5.2 内核卡行字面）：
 * `[display_name][已安装 Badge+version｜未安装][运行中 filled-primary Badge][安装(version 可选 Input)][切换(danger confirm)][重启]`。
 * 数据源恒为后端注册表 status.kernels（能力表单一真源，禁前端内核特例分支）；
 * 换核确认含协议兼容性预检（02§7.3）：supported_kinds × 现节点交集外计数先行上报。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  name: { minWidth: "132px" },
  ver: { width: "120px" },
});

export default function KernelSection({
  st,
  nodes,
  busy,
  onInstall,
  onSelect,
  onRestart,
  onWintun,
}: {
  st: ProxyStatusDto | null;
  nodes: ProxyNodeDto[];
  busy: string;
  onInstall: (kernel: string, version?: string) => Promise<unknown>;
  onSelect: (kernel: string) => Promise<unknown>;
  onRestart: () => Promise<unknown>;
  onWintun: () => Promise<unknown>;
}) {
  const styles = useStyles();
  const [versions, setVersions] = useState<Record<string, string>>({});

  const switchTo = async (k: ProxyKernelInfoDto) => {
    const unsupported = nodes.filter((n) => !k.supported_kinds.includes(n.kind)).length;
    if (
      !(await confirmAction({
        title: "切换内核",
        danger: true,
        command: k.id,
        impact: [
          `将把代理内核切换为「${k.display_name}」`,
          unsupported > 0
            ? `${unsupported} 个节点该内核不支持（切换后这些节点无法作为出口）`
            : "现有节点协议该内核全部支持",
          st?.kernel_running
            ? "运行中将停旧起新（有短暂中断），起新核失败会自动回滚原内核"
            : "当前未运行，选择将在下次启动时生效",
        ],
        detail:
          "能力表来自后端内核注册表（单一真源）：TUN 等不支持的能力对应按钮会如实禁用，不出现「能看见点不动」。",
        confirmLabel: "切换",
      }))
    )
      return;
    await onSelect(k.id);
  };

  return (
    <Section
      title="内核"
      actions={
        <>
          {st?.wintun_installed && <Badge appearance="outline">wintun 已装</Badge>}
          {busy === "kernel-install" && (
            <span className={styles.muted}>正在从官方 Release 下载…</span>
          )}
        </>
      }
    >
      {st?.kernels.map((k) => (
        <div key={k.id} className={styles.row}>
          <Text size={300} weight="semibold" className={styles.name}>
            {k.display_name}
          </Text>
          {k.installed ? (
            <Badge appearance="outline">v{k.version ?? "?"}</Badge>
          ) : (
            <Badge appearance="outline" color="warning">
              未安装
            </Badge>
          )}
          {k.running && (
            <Badge appearance="filled" color="brand">
              运行中
            </Badge>
          )}
          {k.id === st.kernel && (
            <Badge appearance="outline" color="brand">
              当前
            </Badge>
          )}
          <span className={styles.muted}>
            协议 {k.supported_kinds.join("/")} · TUN {k.caps.tun ? "支持" : "不支持"}
          </span>
          <Input
            size="small"
            className={styles.ver}
            placeholder="版本（可选）"
            value={versions[k.id] ?? ""}
            onChange={(_, d) => setVersions((v) => ({ ...v, [k.id]: d.value }))}
          />
          <Button
            size="small"
            disabled={busy !== ""}
            onClick={() =>
              void onInstall(k.id, (versions[k.id] ?? "").trim() || undefined)
            }
          >
            {k.installed ? "重新安装" : "安装"}
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || k.id === st.kernel || !k.installed}
            title={
              k.id === st.kernel
                ? "已是当前内核"
                : !k.installed
                  ? "该内核未安装：先安装后切换"
                  : "切换后按当前模式停旧起新，失败自动回滚"
            }
            onClick={() => void switchTo(k)}
          >
            切换
          </Button>
          <Button
            size="small"
            disabled={busy !== "" || !k.running}
            title={k.running ? "按当前模式重启该内核" : "仅运行中的内核可重启"}
            onClick={() => void onRestart()}
          >
            重启
          </Button>
        </div>
      ))}
      <div className={styles.row}>
        <Button
          size="small"
          disabled={busy !== "" || st?.wintun_installed === true}
          onClick={() => void onWintun()}
        >
          安装 wintun.dll（TUN 前置）
        </Button>
      </div>
      <span className={styles.muted}>
        内核按需下载，不随软件分发；仅支持本地编排，不内置任何节点/订阅。
      </span>
    </Section>
  );
}
