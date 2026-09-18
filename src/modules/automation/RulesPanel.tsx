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

/**
 * 自动化面板（docs/impl/07 A1–A3，M14 v1）：
 * - 规则：事件/启动/每日定时触发 + 可选 when 条件 + 单动作（通知/打开 URL/发布事件）+ 冷却
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
  grow: { flex: 1, minWidth: "120px" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  ok: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
  tab: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "4px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    cursor: "pointer",
    fontSize: tokens.fontSizeBase200,
  },
  tabActive: {
    backgroundColor: tokens.colorNeutralBackground3Hover,
    border: `1px solid ${tokens.colorBrandForeground1}`,
  },
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

/** 表单状态（简化编辑：单动作 + 可选单条件） */
interface FormState {
  id: string;
  name: string;
  trigger: "event" | "startup" | "schedule";
  topic: string;
  time: string;
  useWhen: boolean;
  whenPath: string;
  whenCmp: "eq" | "ne" | "gt" | "lt" | "contains";
  whenValue: string;
  action: "notify" | "open_url" | "publish" | "run_script";
  notifyTitle: string;
  notifyBody: string;
  url: string;
  pubTopic: string;
  pubPayload: string;
  wasmPath: string;
  wasmFunc: string;
  cooldown: string;
}

const EMPTY_FORM: FormState = {
  id: "",
  name: "",
  trigger: "event",
  topic: "clipboard.captured",
  time: "08:30",
  useWhen: false,
  whenPath: "",
  whenCmp: "eq",
  whenValue: "",
  action: "notify",
  notifyTitle: "",
  notifyBody: "",
  url: "",
  pubTopic: "",
  pubPayload: "{}",
  wasmPath: "",
  wasmFunc: "",
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

/** 表单 → DTO（value 尝试 JSON 解析，失败按字符串） */
function formToRule(f: FormState): RuleDto {
  const on: TriggerDto =
    f.trigger === "event"
      ? { kind: "event", topic: f.topic.trim() }
      : f.trigger === "schedule"
        ? { kind: "schedule", time: f.time.trim() }
        : { kind: "startup" };
  const parseValue = (raw: string): unknown => {
    try {
      return JSON.parse(raw);
    } catch {
      return raw;
    }
  };
  let when: ExprDto | null = null;
  if (f.useWhen && f.whenPath.trim()) {
    when = {
      op: "leaf",
      args: { path: f.whenPath.trim(), cmp: f.whenCmp, value: parseValue(f.whenValue) },
    };
  }
  const action: ActionDto =
    f.action === "notify"
      ? { kind: "notify", title: f.notifyTitle, body: f.notifyBody }
      : f.action === "open_url"
        ? { kind: "open_url", url: f.url.trim() }
        : f.action === "run_script"
          ? { kind: "run_script", path: f.wasmPath.trim(), func: f.wasmFunc.trim() }
          : { kind: "publish", topic: f.pubTopic.trim(), payload: parseValue(f.pubPayload || "{}") };
  return {
    id: f.id || crypto.randomUUID(),
    name: f.name.trim(),
    on,
    when,
    then: [action],
    cooldown_secs: Number(f.cooldown) > 0 ? Number(f.cooldown) : 0,
    enabled: true,
  };
}

/** DTO → 表单（编辑回填：取第一个动作/条件） */
function ruleToForm(r: RuleDto): FormState {
  const f: FormState = { ...EMPTY_FORM, id: r.id, name: r.name, cooldown: String(r.cooldown_secs) };
  if (r.on.kind === "event") {
    f.trigger = "event";
    f.topic = r.on.topic;
  } else if (r.on.kind === "schedule") {
    f.trigger = "schedule";
    f.time = r.on.time;
  } else {
    f.trigger = "startup";
  }
  if (r.when && r.when.op === "leaf") {
    f.useWhen = true;
    f.whenPath = r.when.args.path;
    f.whenCmp = r.when.args.cmp;
    f.whenValue = typeof r.when.args.value === "string" ? r.when.args.value : JSON.stringify(r.when.args.value);
  }
  const a = r.then[0];
  if (a) {
    if (a.kind === "notify") {
      f.action = "notify";
      f.notifyTitle = a.title;
      f.notifyBody = a.body;
    } else if (a.kind === "open_url") {
      f.action = "open_url";
      f.url = a.url;
    } else if (a.kind === "publish") {
      f.action = "publish";
      f.pubTopic = a.topic;
      f.pubPayload = JSON.stringify(a.payload);
    } else if (a.kind === "run_script") {
      f.action = "run_script";
      f.wasmPath = a.path;
      f.wasmFunc = a.func;
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
        automationPluginsList().catch(() => [] as PluginInfoDto[]),
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

  const save = async () => {
    if (!form) return;
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

  const remove = async (id: string) => {
    try {
      await automationDeleteRule(id);
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

  const removePlugin = async (id: string) => {
    try {
      await automationPluginRemove(id);
      setNotice("插件已删除");
      await load();
    } catch (e) {
      setError(parseAppError(e)?.data.message ?? String(e));
    }
  };

  return (
    <div className={styles.root}>
      <div className={styles.row}>
        <button className={`${styles.tab} ${tab === "rules" ? styles.tabActive : ""}`} onClick={() => setTab("rules")}>
          规则（{rules.length}）
        </button>
        <button className={`${styles.tab} ${tab === "dead" ? styles.tabActive : ""}`} onClick={() => setTab("dead")}>
          死信（{dead.length}）
        </button>
        <button
          className={`${styles.tab} ${tab === "plugins" ? styles.tabActive : ""}`}
          onClick={() => setTab("plugins")}
        >
          插件（{plugins.length}）
        </button>
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

      {error && <Text className={styles.error}>{error}</Text>}
      {!error && notice && <Text className={styles.ok}>{notice}</Text>}
      {loading ? (
        <Spinner size="tiny" />
      ) : tab === "rules" ? (
        <>
          {form && (
            <div className={styles.section}>
              <Text weight="semibold">{form.id ? "编辑规则" : "新建规则"}</Text>
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
                <Switch checked={form.useWhen} onChange={(_, d) => set({ useWhen: d.checked })} label="启用条件过滤（when）" />
                {form.useWhen && (
                  <>
                    <Input size="small" value={form.whenPath} onChange={(_, d) => set({ whenPath: d.value })} placeholder="payload 点路径，如 entry.kind" className={styles.grow} />
                    <select
                      className={styles.select}
                      value={form.whenCmp}
                      onChange={(e) => set({ whenCmp: e.target.value as FormState["whenCmp"] })}
                    >
                      <option value="eq">等于</option>
                      <option value="ne">不等于</option>
                      <option value="gt">大于</option>
                      <option value="lt">小于</option>
                      <option value="contains">包含</option>
                    </select>
                    <Input size="small" value={form.whenValue} onChange={(_, d) => set({ whenValue: d.value })} placeholder="比较值（数字/字符串/JSON）" className={styles.grow} />
                  </>
                )}
              </div>
              <div className={styles.row}>
                <div className={styles.field}>
                  <Text className={styles.label}>动作</Text>
                  <select
                    className={styles.select}
                    value={form.action}
                    onChange={(e) => set({ action: e.target.value as FormState["action"] })}
                  >
                    <option value="notify">前端通知</option>
                    <option value="open_url">打开 URL/路径</option>
                    <option value="publish">发布事件</option>
                    <option value="run_script">执行 WASM 插件</option>
                  </select>
                </div>
                {form.action === "notify" && (
                  <>
                    <div className={styles.field}>
                      <Text className={styles.label}>标题</Text>
                      <Input size="small" value={form.notifyTitle} onChange={(_, d) => set({ notifyTitle: d.value })} />
                    </div>
                    <div className={styles.field}>
                      <Text className={styles.label}>正文</Text>
                      <Input size="small" value={form.notifyBody} onChange={(_, d) => set({ notifyBody: d.value })} />
                    </div>
                  </>
                )}
                {form.action === "open_url" && (
                  <div className={styles.field}>
                    <Text className={styles.label}>URL / 路径</Text>
                    <Input size="small" value={form.url} onChange={(_, d) => set({ url: d.value })} placeholder="https:// 或 C:\path" />
                  </div>
                )}
                {form.action === "publish" && (
                  <>
                    <div className={styles.field}>
                      <Text className={styles.label}>目标主题（需在 TOPIC_REGISTRY 登记）</Text>
                      <Input size="small" value={form.pubTopic} onChange={(_, d) => set({ pubTopic: d.value })} />
                    </div>
                    <div className={styles.field}>
                      <Text className={styles.label}>payload（JSON）</Text>
                      <Input size="small" value={form.pubPayload} onChange={(_, d) => set({ pubPayload: d.value })} />
                    </div>
                  </>
                )}
                {form.action === "run_script" && (
                  <>
                    <div className={styles.field}>
                      <Text className={styles.label}>插件（plugin:id 或 wasm 路径）</Text>
                      <Input size="small" value={form.wasmPath} onChange={(_, d) => set({ wasmPath: d.value })} placeholder="plugin:demo" />
                    </div>
                    <div className={styles.field}>
                      <Text className={styles.label}>入口函数（空=manifest.func）</Text>
                      <Input size="small" value={form.wasmFunc} onChange={(_, d) => set({ wasmFunc: d.value })} placeholder="run" />
                    </div>
                  </>
                )}
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
            </div>
          )}

          <div className={styles.section}>
            <div className={styles.row}>
              <Text weight="semibold">规则清单</Text>
              <Badge appearance="outline">触发时求值 when → 冷却通过 → 串行执行</Badge>
            </div>
            {rules.length === 0 ? (
              <Text className={styles.muted}>暂无规则——点击「新建规则」创建第一条自动化。</Text>
            ) : (
              <div className={styles.list}>
                {rules.map((r) => (
                  <div key={r.id} className={styles.item}>
                    <div className={styles.itemBody}>
                      <div className={styles.row}>
                        <Text weight="semibold" size={300}>
                          {r.name}
                        </Text>
                        <Badge appearance="outline">{triggerLabel(r.on)}</Badge>
                        <Badge appearance="outline">{actionLabel(r.then[0])}</Badge>
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
                    <Button size="small" appearance="subtle" onClick={() => void remove(r.id)}>
                      删除
                    </Button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </>
      ) : tab === "dead" ? (
        <div className={styles.section}>
          <div className={styles.row}>
            <Text weight="semibold">死信队列</Text>
            <Badge appearance="outline">动作重试耗尽后进入此处</Badge>
          </div>
          {dead.length === 0 ? (
            <Text className={styles.muted}>队列为空——所有动作执行成功。</Text>
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
        </div>
      ) : (
        <div className={styles.section}>
          <div className={styles.row}>
            <Text weight="semibold">插件管理</Text>
            <Badge appearance="outline">wasmtime 沙箱 · 64MB 内存 + fuel 限额</Badge>
          </div>
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
            <Text className={styles.muted}>暂无已安装插件。</Text>
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
                  <Button size="small" appearance="subtle" onClick={() => void removePlugin(p.id)}>
                    删除
                  </Button>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
