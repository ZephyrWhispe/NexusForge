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
 * 节点子面板（T-B2-3 迁移自旧「节点」块；分组 Tab/收藏/行操作列归 T-B2-11 与 B7）：
 * 现表 = 名称/协议/地址/TCP 延迟。
 */
const useStyles = makeStyles({
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
});

export default function NodesSection({
  nodes,
  delays,
  busy,
  loaded,
  onTestDelays,
}: {
  nodes: ProxyNodeDto[];
  delays: Record<string, number | null>;
  busy: string;
  loaded: boolean;
  onTestDelays: () => void;
}) {
  const styles = useStyles();
  return (
    <Section
      title="节点"
      actions={
        <>
          <Button size="small" disabled={busy !== "" || nodes.length === 0} onClick={onTestDelays}>
            {busy === "delay" ? "测速中…" : "测速（TCP）"}
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
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      )}
    </Section>
  );
}
