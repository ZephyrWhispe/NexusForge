import {
  makeStyles,
  tokens,
  Badge,
  Button,
  Table,
  TableBody,
  TableCell,
  TableRow,
} from "@fluentui/react-components";
import type { ProxyNodeDto } from "../../../ipc/client";
import Section from "../../../components/Section";
import EmptyState from "../../../components/EmptyState";
import DeferredBadge from "../../../components/DeferredBadge";

/**
 * 节点子面板（T-B2-3 迁移自旧「节点」块；T-B2-11 出口选点行 + 单节点测速；
 * 收藏/HTTP 测速归 B7）：现表 = 名称/协议/地址/TCP 延迟/出口操作。
 */
const useStyles = makeStyles({
  row: { display: "flex", alignItems: "center", gap: "8px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
});

export default function NodesSection({
  nodes,
  delays,
  busy,
  loaded,
  onTestDelays,
  selected,
  selectedStale,
  onSelectNode,
  onNodeAuto,
  onTestOne,
}: {
  nodes: ProxyNodeDto[];
  delays: Record<string, number | null>;
  busy: string;
  loaded: boolean;
  onTestDelays: () => void;
  /** 手动选定的出口 [sub_id, tag]；null = 自动（urltest 组自选） */
  selected: [string, string] | null;
  /** 选定节点已被订阅更新删除（消费侧回落首节点，UI 如实标注） */
  selectedStale: boolean;
  onSelectNode: (subId: string, tag: string) => void;
  onNodeAuto: () => void;
  onTestOne: (subId: string, tag: string) => void;
}) {
  const styles = useStyles();
  return (
    <Section
      title="节点"
      actions={
        <>
          <Badge
            appearance={selected ? (selectedStale ? "outline" : "filled") : "outline"}
            color={selected ? (selectedStale ? "danger" : "brand") : "subtle"}
          >
            {selected
              ? `手动出口: ${selected[1]}${selectedStale ? "（已失效，实际走自动优选）" : ""}`
              : "自动出口（urltest）"}
          </Badge>
          {selected && (
            <Button size="small" disabled={busy !== ""} onClick={onNodeAuto}>
              {busy === "node-auto" ? "切换中…" : "切回自动"}
            </Button>
          )}
          <Button size="small" disabled={busy !== "" || nodes.length === 0} onClick={onTestDelays}>
            {busy === "delay" ? "测速中…" : "测速全部（TCP）"}
          </Button>
          <DeferredBadge label="逐节点 HTTP 测速" decisionRef="B7" />
        </>
      }
    >
      {nodes.length === 0 ? (
        <EmptyState text="暂无节点：请先添加订阅并拉取" loading={!loaded} />
      ) : (
        <Table size="small">
          <TableBody>
            {nodes.map((n) => {
              const ms = delays[`${n.sub_id}|${n.tag}`];
              const isSel = selected != null && selected[0] === n.sub_id && selected[1] === n.tag;
              return (
                <TableRow key={`${n.sub_id}|${n.tag}`}>
                  <TableCell>
                    <span className={styles.mono}>{n.tag}</span>
                    {/* T-B2-8 组名过滤列（订阅内嵌 proxy-groups 归属；URI 订阅恒空即不渲染） */}
                    {n.groups.length > 0 && (
                      <div className={styles.muted}>组: {n.groups.join(" · ")}</div>
                    )}
                  </TableCell>
                  <TableCell>{n.kind}</TableCell>
                  <TableCell>
                    <span className={styles.mono}>
                      {n.server}:{n.port}
                    </span>
                  </TableCell>
                  <TableCell>
                    {ms === undefined ? (
                      <span className={styles.muted}>—</span>
                    ) : ms === null ? (
                      <Badge appearance="outline" color="danger">
                        不可达
                      </Badge>
                    ) : (
                      <Badge appearance="outline" color="success">
                        {ms} ms
                      </Badge>
                    )}
                  </TableCell>
                  <TableCell>
                    <span className={styles.row}>
                      {isSel ? (
                        <Badge
                          appearance="filled"
                          color={selectedStale ? "danger" : "brand"}
                          title={selectedStale ? "该节点已被订阅更新删除：实际出口回落自动优选" : undefined}
                        >
                          出口
                        </Badge>
                      ) : (
                        <Button
                          size="small"
                          disabled={busy !== ""}
                          onClick={() => onSelectNode(n.sub_id, n.tag)}
                        >
                          {busy === "node-select" ? "切换中…" : "选定"}
                        </Button>
                      )}
                      <Button
                        size="small"
                        disabled={busy !== ""}
                        onClick={() => onTestOne(n.sub_id, n.tag)}
                      >
                        {busy === `delay-${n.sub_id}|${n.tag}` ? "测速中…" : "TCP 测"}
                      </Button>
                    </span>
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
    </Section>
  );
}
