import { useCallback, useEffect, useState } from "react";
import {
  makeStyles,
  tokens,
  Text,
  Badge,
  Button,
  Input,
  Switch,
  Spinner,
} from "@fluentui/react-components";
import {
  automationDeadLetters,
  automationDeleteRule,
  automationPluginInstall,
  automationPluginRemove,
  automationPluginsList,
  automationReplay,
  automationRulesList,
  automationSaveRule,
  automationToggleRule,
  parseAppError,
  type ActionDto,
  type ExprDto,
  type PluginInfoDto,
  type RuleDto,
  type TriggerDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { confirmAction } from "../../stores/confirm";
import Section from "../../components/Section";
import Tabs from "../../components/Tabs";
import InlineError from "../../components/InlineError";
import EmptyState from "../../components/EmptyState";

/**
 * 自动化面板（docs/impl/07 A1–A3 + T-B7-13 多动作编辑器）：
 * - 规则：事件/启动/每日定时触发 + 可选 when（一层 And/Or 组 UI；深层树逐字
 *   携带不回造，触碰降级须显式确认）+ **then 数组**（增删/上下移/五类动作全
 *   含 ipc_command）+ 冷却
 * - 死信：动作重试耗尽的死信队列（可重放；重放仍失败以新 id 重新入队）
 * - 风暴防护：automation 自产事件不再触发规则（防自环）；默认冷却 5s/规则
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
  grow: { flex: 1, minWidth: "120px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
    maxHeight: "380px",
    overflowY: "auto",
  },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  itemBody: { flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "2px" },
  field: { display: "flex", flexDirection: "column", gap: "4px", flex: 1, minWidth: "160px" },
  label: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground2 },
  select: {
    padding: "5px 8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    fontSize: tokens.fontSizeBase200,
  },
  dead: {
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
  },
});

type TabId = "rules" | "dead" | "plugins";

type ActionKind = "notify" | "open_url" | "publish" | "ipc_command" | "run_script";

/** 动作行（T-B7-13 then 数组编辑器）：五类参数字段并存一行形状，按 kind 显形 */
interface ActionRow {
  kind: ActionKind;
  notifyTitle: string;
  notifyBody: string;
  url: string;
  pubTopic: string;
  pubPayload: string;
  ipcModule: string;
  ipcCmd: string;
  ipcArgs: string;
  wasmPath: string;
  wasmFunc: string;
}

/** when 叶行（UI 只造一层；深层树走 rawWhen 逐字携带） */
interface WhenLeafRow {
  path: string;
  cmp: "eq" | "ne" | "gt" | "lt" | "contains";
  value: string;
}

interface FormState {
  id: string;
  name: string;
  trigger: "event" | "startup" | "schedule";
  topic: string;
  time: string;
  /** none = 无条件 | leaf = 单条件 | group = 一层 And/Or 组 */
  whenMode: "none" | "leaf" | "group";
  whenGroupOp: "and" | "or";
  whenLeaves: WhenLeafRow[];
  /** 载入即存的深层树：未触碰原样序列化回去；触碰才降级 + 保存前显式确认 */
  rawWhen: ExprDto | null;
  whenTouched: boolean;
  /** then 整数组（编辑回填不回造——每动作一行，序即执行序） */
  actions: ActionRow[];
  /** 启停随表单往返（编辑停用规则保存后不得被悄悄启用） */
  enabled: boolean;
  cooldown: string;
}

const EMPTY_ROW: ActionRow = {
  kind: "notify",
  notifyTitle: "",
  notifyBody: "",
  url: "",
  pubTopic: "",
  pubPayload: "{}",
  ipcModule: "",
  ipcCmd: "",
  ipcArgs: "",
  wasmPath: "",
  wasmFunc: "",
};

const EMPTY_FORM: FormState = {
  id: "",
  name: "",
  trigger: "event",
  topic: "clipboard.captured",
  time: "08:30",
  whenMode: "none",
  whenGroupOp: "and",
  whenLeaves: [],
  rawWhen: null,
  whenTouched: false,
  actions: [{ ...EMPTY_ROW }],
  enabled: true,
  cooldown: "0",
};

/** 触发摘要（列表展示） */
function triggerLabel(on: TriggerDto): string {
  switch (on.kind) {
    case "event":
      return `事件 · ${on.topic}`;
    case "startup":
      return "应用启动";
    case "schedule":
      return `每日 ${on.time}`;
  }
}

/** 动作摘要 */
function actionLabel(a: ActionDto): string {
  switch (a.kind) {
    case "notify":
      return `通知 · ${a.title}`;
    case "open_url":
      return `打开 · ${a.url}`;
    case "publish":
      return `发布 · ${a.topic}`;
    case "ipc_command":
      return `IPC · ${a.module}.${a.cmd}`;
    case "run_script":
      return `插件 · ${a.path}${a.func ? `:${a.func}` : ""}`;
  }
}

const parseValue = (raw: string): unknown => {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
};

/** 动作行 → DTO；ipc_command 参数 JSON 解析失败回 err（就地红、禁提交） */
function rowToAction(r: ActionRow): { action?: ActionDto; err?: string } {
  switch (r.kind) {
    case "notify":
      return { action: { kind: "notify", title: r.notifyTitle, body: r.notifyBody } };
    case "open_url":
      return { action: { kind: "open_url", url: r.url.trim() } };
    case "publish":
      return {
        action: {
          kind: "publish",
          topic: r.pubTopic.trim(),
          payload: parseValue(r.pubPayload || "{}"),
        },
      };
    case "run_script":
      return { action: { kind: "run_script", path: r.wasmPath.trim(), func: r.wasmFunc.trim() } };
    case "ipc_command": {
      const raw = r.ipcArgs.trim();
      let args: unknown = {};
      if (raw !== "") {
        try {
          args = JSON.parse(raw);
        } catch {
          return { err: "IPC 参数不是合法 JSON，修正后才能保存" };
        }
      }
      return { action: { kind: "ipc_command", module: r.ipcModule.trim(), cmd: r.ipcCmd.trim(), args } };
    }
  }
}

/** 动作 DTO → 编辑行（回填不回造：字段逐字带回） */
function rowFromAction(a: ActionDto): ActionRow {
  const base = { ...EMPTY_ROW };
  switch (a.kind) {
    case "notify":
      return { ...base, kind: "notify", notifyTitle: a.title, notifyBody: a.body };
    case "open_url":
      return { ...base, kind: "open_url", url: a.url };
    case "publish":
      return { ...base, kind: "publish", pubTopic: a.topic, pubPayload: JSON.stringify(a.payload) };
    case "ipc_command":
      return {
        ...base,
        kind: "ipc_command",
        ipcModule: a.module,
        ipcCmd: a.cmd,
        ipcArgs: JSON.stringify(a.args),
      };
    case "run_script":
      return { ...base, kind: "run_script", wasmPath: a.path, wasmFunc: a.func };
  }
}

const leafValueText = (v: unknown): string =>
  typeof v === "string" ? v : JSON.stringify(v);

function whenLeavesOf(rows: ExprDto[]): WhenLeafRow[] {
  return rows.flatMap((e) =>
    e.op === "leaf"
      ? [{ path: e.args.path, cmp: e.args.cmp, value: leafValueText(e.args.value) }]
      : [],
  );
}

const EMPTY_LEAF: WhenLeafRow = { path: "", cmp: "eq", value: "" };

const withLeaf = (
  rows: WhenLeafRow[],
  i: number,
  patch: Partial<WhenLeafRow>,
): WhenLeafRow[] => rows.map((row, idx) => (idx === i ? { ...row, ...patch } : row));

/** 表单 → when DTO（编辑形状；rawWhen 的取舍在 formToRule 收口） */
function whenFromForm(f: FormState): ExprDto | null {
  if (f.whenMode === "none") return null;
  const leaves = f.whenLeaves
    .filter((row) => row.path.trim() !== "")
    .map<ExprDto>((row) => ({
      op: "leaf",
      args: { path: row.path.trim(), cmp: row.cmp, value: parseValue(row.value) },
    }));
  if (leaves.length === 0) return null;
  if (f.whenMode === "leaf") return leaves.find(() => true) ?? null;
  return { op: f.whenGroupOp, args: leaves };
}

/** 表单 → DTO：深层 when 未触碰 → 逐字原样携带（正对照防"编辑器必然展平"） */
function formToRule(f: FormState): RuleDto {
  const on: TriggerDto =
    f.trigger === "event"
      ? { kind: "event", topic: f.topic.trim() }
      : f.trigger === "schedule"
        ? { kind: "schedule", time: f.time.trim() }
        : { kind: "startup" };
  const when = f.rawWhen !== null && !f.whenTouched ? f.rawWhen : whenFromForm(f);
  return {
    id: f.id || crypto.randomUUID(),
    name: f.name.trim(),
    on,
    when,
    then: f.actions.flatMap((row) => {
      const built = rowToAction(row);
      return built.action ? [built.action] : [];
    }),
    cooldown_secs: Number(f.cooldown) > 0 ? Number(f.cooldown) : 0,
    enabled: f.enabled,
  };
}

/** DTO → 表单（编辑回填：then 整数组 + when 一层/深层树分臂载入） */
function ruleToForm(r: RuleDto): FormState {
  const f: FormState = {
    ...EMPTY_FORM,
    id: r.id,
    name: r.name,
    cooldown: String(r.cooldown_secs),
    enabled: r.enabled,
    actions: r.then.map(rowFromAction),
  };
  if (r.on.kind === "event") {
    f.trigger = "event";
    f.topic = r.on.topic;
  } else if (r.on.kind === "schedule") {
    f.trigger = "schedule";
    f.time = r.on.time;
  } else {
    f.trigger = "startup";
  }
  if (r.when) {
    if (r.when.op === "leaf") {
      f.whenMode = "leaf";
      f.whenLeaves = whenLeavesOf([r.when]);
    } else if (
      (r.when.op === "and" || r.when.op === "or") &&
      r.when.args.every((e) => e.op === "leaf")
    ) {
      // 恰好一层（成员全 leaf）：可安全往返，进组编辑器
      f.whenMode = "group";
      f.whenGroupOp = r.when.op;
      f.whenLeaves = whenLeavesOf(r.when.args);
    } else {
      // 深层树（嵌套组/not）：逐字存 rawWhen，编辑器不碰它
      f.rawWhen = r.when;
    }
  }
  return f;
}

export default function RulesPanel() {
  const styles = useStyles();
  const [tab, setTab] = useState<TabId>("rules");
  const [rules, setRules] = useState<RuleDto[]>([]);
  const [dead, setDead] = useState<import("../../ipc/client").DeadLetterDto[]>([]);
  const [plugins, setPlugins] = useState<PluginInfoDto[]>([]);
  const [installDir, setInstallDir] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [form, setForm] = useState<FormState | null>(null);

  const load = useCallback(async () => {
    try {
      const [rs, ds, ps] = await Promise.all([
        automationRulesList(),
        automationDeadLetters(),
        automationPluginsList().catch((e) => {
          reportError(e, { context: "插件列表加载失败（已降级为空）", dedupeKey: "plugins-list", toast: false });
          return [] as PluginInfoDto[];
        }),
      ]);
      setRules(rs);
      setDead(ds);
      setPlugins(ps);
      setError("");
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const set = (patch: Partial<FormState>) => setForm((f) => (f ? { ...f, ...patch } : f));

  /** when 区的任何编辑都算「触碰」：rawWhen 逐字携带让位给编辑器形状（保存前有确认闸） */
  const markWhen = (patch: Partial<FormState>) =>
    setForm((f) => (f ? { ...f, ...patch, whenTouched: true } : f));

  const setRow = (i: number, patch: Partial<ActionRow>) =>
    setForm((f) =>
      f ? { ...f, actions: f.actions.map((row, idx) => (idx === i ? { ...row, ...patch } : row)) } : f
    );

  const moveRow = (i: number, dir: -1 | 1) =>
    setForm((f) => {
      if (!f) return f;
      const j = i + dir;
      if (j < 0 || j >= f.actions.length) return f;
      const rows = [...f.actions];
      const tmp = rows[i];
      rows[i] = rows[j];
      rows[j] = tmp;
      return { ...f, actions: rows };
    });

  const save = async () => {
    if (!form) return;
    // IPC 参数非法 JSON：就地红已在各行的错误文本上，禁提交
    if (form.actions.some((row) => rowToAction(row).err)) return;
    if (form.rawWhen !== null && form.whenTouched) {
      // 深层树展平是破坏性语义变更（D-18 纪律）：静默裁切改显式确认
      const ok = await confirmAction({
        title: "嵌套条件展平为单层",
        impact: [
          "原规则 when 为深层嵌套树（多层组 / not 条件），编辑器已将其展平",
          "保存后条件将变为当前单层 And/Or 形状，原嵌套结构丢失",
        ],
        detail: "取消则回到表单，深层树仍逐字保留不丢。",
        confirmLabel: "展平并保存",
      });
      if (!ok) return;
    }
    try {
      await automationSaveRule(formToRule(form));
      setForm(null);
      setNotice("规则已保存");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const toggle = async (id: string, enabled: boolean) => {
    try {
      await automationToggleRule(id, enabled);
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const remove = async (r: RuleDto) => {
    // 破坏性操作（D-18）：点名规则 + 触发/动作影响面，确认后再删
    if (
      !(await confirmAction({
        title: "删除规则",
        impact: [
          `将删除规则「${r.name}」`,
          `触发：${triggerLabel(r.on)} · 动作 ${r.then.length} 个${r.when ? " · 含 when 条件" : ""}`,
        ],
        detail: "规则定义将从自动化引擎卸载，历史死信不受影响；如需再次使用须重新创建。",
        confirmLabel: "删除",
      }))
    )
      return;
    try {
      await automationDeleteRule(r.id);
      setNotice("规则已删除");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const replay = async (deadId: string, ruleId: string) => {
    try {
      await automationReplay(deadId, ruleId);
      setNotice("重放完成（仍失败会以新死信保留）");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const installPlugin = async () => {
    if (!installDir.trim()) return;
    try {
      const m = await automationPluginInstall(installDir.trim());
      setNotice(`插件 ${m.id} 安装成功`);
      setInstallDir("");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  const removePlugin = async (p: PluginInfoDto) => {
    // 破坏性操作（D-18）：点名插件 + 引用它、将失效的规则动作数
    const refs = rules.filter((r) =>
      r.then.some((a) => a.kind === "run_script" && a.path === `plugin:${p.id}`),
    ).length;
    if (
      !(await confirmAction({
        title: "删除插件",
        impact: [
          `将卸载插件「${p.name}」（${p.id} · v${p.version}）`,
          `${refs} 条规则引用 plugin:${p.id}`,
        ],
        detail: refs
          ? "删除后上述规则的「执行 WASM 插件」动作将失败；插件目录与 wasm 一并移除，不可恢复。"
          : "删除插件目录与 wasm 文件，不可恢复；如需再次使用须重新安装。",
        confirmLabel: "删除",
      }))
    )
      return;
    try {
      await automationPluginRemove(p.id);
      setNotice("插件已删除");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <Tabs
          ariaLabel="自动化视图"
          value={tab}
          onChange={setTab}
          items={[
            { id: "rules", label: `规则（${rules.length}）` },
            { id: "dead", label: `死信（${dead.length}）` },
            { id: "plugins", label: `插件（${plugins.length}）` },
          ]}
        />
        <div style={{ flex: 1 }} />
        {tab === "rules" && (
          <Button appearance="primary" size="small" onClick={() => setForm({ ...EMPTY_FORM })}>
            新建规则
          </Button>
        )}
        <Button size="small" onClick={() => void load()}>
          刷新
        </Button>
      </div>

      <InlineError text={error} />
      <InlineError text={notice} tone="success" />
      {loading ? (
        <Spinner size="tiny" />
      ) : tab === "rules" ? (
        <>
          {form && (
            <Section title={form.id ? "编辑规则" : "新建规则"}>
              <div className={styles.row}>
                <div className={styles.field}>
                  <Text className={styles.label}>名称</Text>
                  <Input size="small" value={form.name} onChange={(_, d) => set({ name: d.value })} placeholder="规则名" />
                </div>
                <div className={styles.field}>
                  <Text className={styles.label}>触发</Text>
                  <select
                    className={styles.select}
                    value={form.trigger}
                    onChange={(e) => set({ trigger: e.target.value as FormState["trigger"] })}
                  >
                    <option value="event">事件触发</option>
                    <option value="startup">应用启动</option>
                    <option value="schedule">每日定时</option>
                  </select>
                </div>
                {form.trigger === "event" && (
                  <div className={styles.field}>
                    <Text className={styles.label}>事件主题</Text>
                    <Input size="small" value={form.topic} onChange={(_, d) => set({ topic: d.value })} placeholder="clipboard.captured" />
                  </div>
                )}
                {form.trigger === "schedule" && (
                  <div className={styles.field}>
                    <Text className={styles.label}>时间（HH:MM）</Text>
                    <Input size="small" value={form.time} onChange={(_, d) => set({ time: d.value })} placeholder="08:30" />
                  </div>
                )}
                <div className={styles.field}>
                  <Text className={styles.label}>冷却（秒，0=默认 5s）</Text>
                  <Input size="small" type="number" value={form.cooldown} onChange={(_, d) => set({ cooldown: d.value })} />
                </div>
              </div>
              <div className={styles.row}>
                <Switch
                  checked={form.whenMode !== "none" || form.rawWhen !== null}
                  onChange={(_, d) =>
                    d.checked
                      ? markWhen({ whenMode: "leaf", whenLeaves: [{ ...EMPTY_LEAF }] })
                      : markWhen({ whenMode: "none", whenLeaves: [], rawWhen: null })
                  }
                  label="启用条件过滤（when）"
                />
              </div>
              {form.rawWhen !== null && form.whenMode === "none" && (
                <div className={styles.row}>
                  <Text className={styles.muted}>
                    本规则的 when 是深层嵌套条件树：逐字保留，未触碰将原样保存。
                  </Text>
                  <Button
                    size="small"
                    onClick={() =>
                      markWhen({
                        whenMode: "group",
                        whenGroupOp: "and",
                        whenLeaves: [{ ...EMPTY_LEAF }],
                      })
                    }
                  >
                    编辑条件（将降级为单层）
                  </Button>
                </div>
              )}
              {form.whenMode === "group" && (
                <div className={styles.row}>
                  <Button
                    size="small"
                    appearance={form.whenGroupOp === "and" ? "primary" : "subtle"}
                    onClick={() => markWhen({ whenGroupOp: "and" })}
                  >
                    满足全部（and）
                  </Button>
                  <Button
                    size="small"
                    appearance={form.whenGroupOp === "or" ? "primary" : "subtle"}
                    onClick={() => markWhen({ whenGroupOp: "or" })}
                  >
                    满足其一（or）
                  </Button>
                </div>
              )}
              {form.whenMode !== "none" &&
                form.whenLeaves.map((leaf, i) => (
                  <div key={i} className={styles.row}>
                    <Input
                      size="small"
                      value={leaf.path}
                      onChange={(_, d) => markWhen({ whenLeaves: withLeaf(form.whenLeaves, i, { path: d.value }) })}
                      placeholder="payload 点路径，如 entry.kind"
                      className={styles.grow}
                    />
                    <select
                      className={styles.select}
                      value={leaf.cmp}
                      onChange={(e) =>
                        markWhen({
                          whenLeaves: withLeaf(form.whenLeaves, i, {
                            cmp: e.target.value as WhenLeafRow["cmp"],
                          }),
                        })
                      }
                    >
                      <option value="eq">等于</option>
                      <option value="ne">不等于</option>
                      <option value="gt">大于</option>
                      <option value="lt">小于</option>
                      <option value="contains">包含</option>
                    </select>
                    <Input
                      size="small"
                      value={leaf.value}
                      onChange={(_, d) => markWhen({ whenLeaves: withLeaf(form.whenLeaves, i, { value: d.value }) })}
                      placeholder="比较值（数字/字符串/JSON）"
                      className={styles.grow}
                    />
                    {form.whenMode === "group" && form.whenLeaves.length > 1 && (
                      <Button
                        size="small"
                        appearance="subtle"
                        onClick={() =>
                          markWhen({ whenLeaves: form.whenLeaves.filter((_, idx) => idx !== i) })
                        }
                      >
                        删除
                      </Button>
                    )}
                  </div>
                ))}
              {form.whenMode === "leaf" && (
                <div className={styles.row}>
                  <Button
                    size="small"
                    onClick={() => markWhen({ whenMode: "group", whenGroupOp: "and" })}
                  >
                    组成条件组
                  </Button>
                </div>
              )}
              {form.whenMode === "group" && (
                <div className={styles.row}>
                  <Button
                    size="small"
                    onClick={() => markWhen({ whenLeaves: [...form.whenLeaves, { ...EMPTY_LEAF }] })}
                  >
                    添加条件
                  </Button>
                </div>
              )}
              <Text className={styles.label}>动作序列（按序串行执行 · 可上移/下移/增删）</Text>
              {form.actions.map((row, i) => (
                <div key={i} className={styles.row}>
                  <Badge appearance="outline">{i + 1}</Badge>
                  <div className={styles.field}>
                    <select
                      className={styles.select}
                      value={row.kind}
                      onChange={(e) => setRow(i, { kind: e.target.value as ActionKind })}
                    >
                      <option value="notify">前端通知</option>
                      <option value="open_url">打开 URL/路径</option>
                      <option value="publish">发布事件</option>
                      <option value="ipc_command">执行 IPC 命令</option>
                      <option value="run_script">执行 WASM 插件</option>
                    </select>
                  </div>
                  {row.kind === "notify" && (
                    <>
                      <div className={styles.field}>
                        <Text className={styles.label}>标题</Text>
                        <Input size="small" value={row.notifyTitle} onChange={(_, d) => setRow(i, { notifyTitle: d.value })} />
                      </div>
                      <div className={styles.field}>
                        <Text className={styles.label}>正文</Text>
                        <Input size="small" value={row.notifyBody} onChange={(_, d) => setRow(i, { notifyBody: d.value })} />
                      </div>
                    </>
                  )}
                  {row.kind === "open_url" && (
                    <div className={styles.field}>
                      <Text className={styles.label}>URL / 路径</Text>
                      <Input size="small" value={row.url} onChange={(_, d) => setRow(i, { url: d.value })} placeholder="https:// 或 C:\path" />
                    </div>
                  )}
                  {row.kind === "publish" && (
                    <>
                      <div className={styles.field}>
                        <Text className={styles.label}>目标主题（需在 TOPIC_REGISTRY 登记）</Text>
                        <Input size="small" value={row.pubTopic} onChange={(_, d) => setRow(i, { pubTopic: d.value })} />
                      </div>
                      <div className={styles.field}>
                        <Text className={styles.label}>payload（JSON）</Text>
                        <Input size="small" value={row.pubPayload} onChange={(_, d) => setRow(i, { pubPayload: d.value })} />
                      </div>
                    </>
                  )}
                  {row.kind === "ipc_command" && (
                    <>
                      <div className={styles.field}>
                        <Text className={styles.label}>模块</Text>
                        <Input size="small" value={row.ipcModule} onChange={(_, d) => setRow(i, { ipcModule: d.value })} placeholder="模块名，如 clipboard" />
                      </div>
                      <div className={styles.field}>
                        <Text className={styles.label}>命令</Text>
                        <Input size="small" value={row.ipcCmd} onChange={(_, d) => setRow(i, { ipcCmd: d.value })} placeholder="命令名，如 clipboard_get_entry" />
                      </div>
                      <div className={styles.field}>
                        <Text className={styles.label}>参数（JSON，空=无参）</Text>
                        <Input size="small" value={row.ipcArgs} onChange={(_, d) => setRow(i, { ipcArgs: d.value })} placeholder='{"id": "..."}' />
                        {rowToAction(row).err && (
                          <Text style={{ color: tokens.colorStatusDangerForeground1, fontSize: tokens.fontSizeBase200 }}>
                            {rowToAction(row).err}
                          </Text>
                        )}
                      </div>
                    </>
                  )}
                  {row.kind === "run_script" && (
                    <>
                      <div className={styles.field}>
                        <Text className={styles.label}>插件（plugin:id 或 wasm 路径）</Text>
                        <Input size="small" value={row.wasmPath} onChange={(_, d) => setRow(i, { wasmPath: d.value })} placeholder="plugin:demo" />
                      </div>
                      <div className={styles.field}>
                        <Text className={styles.label}>入口函数（空=manifest.func）</Text>
                        <Input size="small" value={row.wasmFunc} onChange={(_, d) => setRow(i, { wasmFunc: d.value })} placeholder="run" />
                      </div>
                    </>
                  )}
                  <Button size="small" disabled={i === 0} onClick={() => moveRow(i, -1)}>
                    ↑
                  </Button>
                  <Button size="small" disabled={i === form.actions.length - 1} onClick={() => moveRow(i, 1)}>
                    ↓
                  </Button>
                  <Button
                    size="small"
                    appearance="subtle"
                    disabled={form.actions.length <= 1}
                    onClick={() => set({ actions: form.actions.filter((_, idx) => idx !== i) })}
                  >
                    删除
                  </Button>
                </div>
              ))}
              <div className={styles.row}>
                <Button
                  size="small"
                  onClick={() => set({ actions: [...form.actions, { ...EMPTY_ROW }] })}
                >
                  添加动作
                </Button>
              </div>
              <div className={styles.row}>
                <Button appearance="primary" size="small" onClick={() => void save()}>
                  保存
                </Button>
                <Button size="small" onClick={() => setForm(null)}>
                  取消
                </Button>
                <Text className={styles.muted}>
                  常用主题：clipboard.captured / clipboard.pasted / ocr.completed / notes.changed / desktop.remind_due
                </Text>
              </div>
            </Section>
          )}

          <Section
            title="规则清单"
            actions={<Badge appearance="outline">触发时求值 when → 冷却通过 → 串行执行</Badge>}
          >
            {rules.length === 0 ? (
              <EmptyState text="暂无规则——点击「新建规则」创建第一条自动化。" />
            ) : (
              <div className={styles.list}>
                {rules.map((r) => {
                  const [first, ...rest] = r.then;
                  return (
                    <div key={r.id} className={styles.item}>
                      <div className={styles.itemBody}>
                        <div className={styles.row}>
                          <Text weight="semibold" size={300}>
                            {r.name}
                          </Text>
                          <Badge appearance="outline">{triggerLabel(r.on)}</Badge>
                          <Badge appearance="outline">
                            {actionLabel(first)}
                            {rest.length > 0 ? ` +${rest.length}` : ""}
                          </Badge>
                          {r.when && <Badge appearance="filled">有条件</Badge>}
                        </div>
                        <Text className={styles.muted}>冷却 {r.cooldown_secs || 5}s · {r.enabled ? "已启用" : "已停用"}</Text>
                      </div>
                      <Switch
                        checked={r.enabled}
                        onChange={(_, d) => void toggle(r.id, d.checked)}
                      />
                      <Button size="small" onClick={() => setForm(ruleToForm(r))}>
                        编辑
                      </Button>
                      <Button size="small" appearance="subtle" onClick={() => void remove(r)}>
                        删除
                      </Button>
                    </div>
                  );
                })}
              </div>
            )}
          </Section>
        </>
      ) : tab === "dead" ? (
        <Section
          title="死信队列"
          actions={<Badge appearance="outline">动作重试耗尽后进入此处</Badge>}
        >
          {dead.length === 0 ? (
            <EmptyState text="队列为空——所有动作执行成功。" />
          ) : (
            <div className={styles.list}>
              {dead.map((d) => (
                <div key={d.id} className={styles.item}>
                  <div className={styles.itemBody}>
                    <Text weight="semibold" size={300}>
                      {d.rule_name}
                    </Text>
                    <Text className={styles.dead}>
                      {actionLabel(d.action)}
                      {"\n"}
                      {d.error}
                      {"\n"}
                      {new Date(d.at_ms).toLocaleString()}
                    </Text>
                  </div>
                  <Button size="small" onClick={() => void replay(d.id, d.rule_id)}>
                    重放
                  </Button>
                </div>
              ))}
            </div>
          )}
        </Section>
      ) : (
        <Section
          title="插件管理"
          actions={<Badge appearance="outline">wasmtime 沙箱 · 64MB 内存 + fuel 限额</Badge>}
        >
          <Text className={styles.muted}>
            安装 = 本地插件目录（含 manifest.json + entry wasm，sha256 校验）；权限 open/notify 映射宿主函数白名单，log 恒可用。规则动作路径填 plugin:{"{id}"}。
          </Text>
          <div className={styles.row}>
            <Input
              size="small"
              className={styles.grow}
              value={installDir}
              onChange={(_, d) => setInstallDir(d.value)}
              placeholder="插件目录路径，如 D:\plugins\demo"
            />
            <Button appearance="primary" size="small" onClick={() => void installPlugin()}>
              从目录安装
            </Button>
          </div>
          {plugins.length === 0 ? (
            <EmptyState text="暂无已安装插件。" />
          ) : (
            <div className={styles.list}>
              {plugins.map((p) => (
                <div key={p.id} className={styles.item}>
                  <div className={styles.itemBody}>
                    <div className={styles.row}>
                      <Text weight="semibold" size={300}>
                        {p.name}
                      </Text>
                      <Badge appearance="outline">{p.id}</Badge>
                      <Badge appearance="outline">v{p.version}</Badge>
                      <Badge appearance="outline">api {p.api_version}</Badge>
                      {p.permissions.map((perm) => (
                        <Badge key={perm} appearance="filled">
                          {perm}
                        </Badge>
                      ))}
                      {!p.installed && <Badge appearance="outline">entry 缺失</Badge>}
                    </div>
                    <Text className={styles.muted}>
                      entry {p.entry} · func {p.func} · sha256 {p.sha256.slice(0, 12)}…
                    </Text>
                  </div>
                  <Button size="small" appearance="subtle" onClick={() => void removePlugin(p)}>
                    删除
                  </Button>
                </div>
              ))}
            </div>
          )}
        </Section>
      )}
    </div>
  );
}
