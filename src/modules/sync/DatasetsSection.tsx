import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  makeStyles,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  parseAppError,
  syncDatasetsGet,
  type SyncDatasetDto,
} from "../../ipc/client";
import Section from "../../components/Section";
import DeferredBadge from "../../components/DeferredBadge";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 数据集页（09 §10.2 T-B5-8）：同步范围的**读面**，白名单与运行态并排说。
 *
 * 数据源是 `sync_datasets_get`（= 编译期 `SYNC_ENTITIES` 白名单 × 当前装了应用器的
 * entity 集合），面板不自带一份设备清单文案——前端硬编码"笔记库/密码库"正是本批次
 * 一路在消除的"两处真源会漂移"：名单换了代、面板还在念旧字面。
 * `attached:false` 那一行如实留白（名单在册但宿主没装应用器 = 半成品状态），
 * 既不隐藏也不美化成"已就绪"。
 * 密码库**不在册**是红线而不是读数：白名单无 vault 条目 + attach 运行期拒 + 分派口
 * 永久拒收（T-B5-5），所以它的文案是"永不自动同步"，与读到的行数是两回事。
 */
const useStyles = makeStyles({
  item: {
    padding: "8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    display: "flex",
    alignItems: "center",
    gap: "8px",
    flexWrap: "wrap",
  },
  grow: { flex: 1, minWidth: "0" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  mono: { fontFamily: "Consolas, monospace", fontSize: tokens.fontSizeBase200 },
});

function attachLabel(d: SyncDatasetDto): { text: string; color: "success" | "warning" } {
  return d.attached
    ? { text: "同步已启用", color: "success" }
    : { text: "在册未接线", color: "warning" };
}

export default function DatasetsSection() {
  const styles = useStyles();
  const [rows, setRows] = useState<SyncDatasetDto[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      setRows(await syncDatasetsGet());
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <Section
      title={`同步数据集（${rows.length}）`}
      actions={
        <>
          <Badge appearance="outline">白名单在册 · 运行态并排可见</Badge>
          <DeferredBadge label="剪贴板数据集" decisionRef="09 §10.3-②" />
          <Button size="small" onClick={() => void load()}>
            刷新
          </Button>
        </>
      }
    >
      <InlineError text={error} />
      {rows.length === 0 ? (
        <EmptyState
          text="同步内核未返回任何在册数据集——这时任何实体都不会被同步。"
          loading={!loaded}
        />
      ) : (
        rows.map((d) => {
          const s = attachLabel(d);
          return (
            <div key={d.id} className={styles.item}>
              <Text weight="semibold" size={300}>
                {d.label}
              </Text>
              <Text className={styles.mono}>{d.id}</Text>
              <div className={styles.grow} />
              <Badge appearance="tint" color={s.color}>
                {s.text}
              </Badge>
            </div>
          );
        })
      )}
      <Text className={styles.muted}>
        在册数据集的增删是同步内核的编译期白名单（SYNC_ENTITIES）决定的能力面，不是这里的开关：
        面板只如实报"名单里有谁、此刻谁在服"。冲突策略：同一篇笔记双向修改按时间戳取最新
        （LWW），败方内容在「冲突」页可查可重新生效。
      </Text>
      <Text className={styles.muted}>
        密码库条目<Text weight="semibold">永不</Text>
        自动同步（仅手动导出加密包）。
      </Text>
    </Section>
  );
}
