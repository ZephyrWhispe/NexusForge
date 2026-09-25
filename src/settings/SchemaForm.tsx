import { useCallback, useEffect, useRef, useState } from "react";
import {
  makeStyles,
  tokens,
  Switch,
  SpinButton,
  Input,
  Textarea,
  Text,
  Dropdown,
  Option,
} from "@fluentui/react-components";
import { hostConfigGet, hostConfigSchema, hostConfigSet } from "../ipc/client";
import { IN_TAURI } from "../ipc/env";

/**
 * SchemaForm（docs/UI-PLAN.md U6-1/U6-3）：
 * 模块 config_schema（JSON Schema）→ Fluent 控件自动渲染；
 * 修改即校验即保存（Rust 侧 schema 校验失败回显错误）。
 * 支持：boolean→Switch；integer(min/max)→SpinButton；string→Input（带 enum 则 Dropdown）；
 * array(string)→Textarea(逗号分隔)。
 *
 * PERF-02：文本/数字/多行输入 400ms 防抖合并提交（此前每击键一次 IPC + 一次落盘，
 * 且触发整链 apply_config 派发）；布尔/下拉等离散控件保持即时提交（点击即意图）。
 * 失败经顶部错误行如实呈现（不吞、不冒充"已保存"）；输入值保留供改正重试。
 */
const DEBOUNCE_MS = 400;
const useStyles = makeStyles({
  root: { flex: 1, overflowY: "auto", padding: "8px 24px 30px", maxWidth: "760px" },
  group: { marginBottom: "22px" },
  title: {
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground2,
    paddingBottom: "8px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    marginBottom: "4px",
    display: "block",
  },
  row: {
    display: "flex",
    alignItems: "center",
    gap: "16px",
    padding: "13px 4px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  info: { flex: 1 },
  name: { display: "block", fontWeight: tokens.fontWeightSemibold },
  desc: { fontSize: tokens.fontSizeBase200, color: tokens.colorNeutralForeground3 },
  err: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
});

interface JsonSchemaProp {
  type: "boolean" | "integer" | "string" | "array";
  title?: string;
  description?: string;
  default?: unknown;
  minimum?: number;
  maximum?: number;
  items?: { type: string };
  /** 有限词表（如 OCR 引擎 id，由后端按注册表动态给出）：有则渲染下拉，杜绝手打字面 */
  enum?: string[];
  /** JSON Schema 2020-12 注解：Rust 侧仍校验该键，但通用表单不渲染，留给专用写口（如捕获暂停开关卡） */
  readOnly?: boolean;
}

type Values = Record<string, unknown>;

/** 通用表单可见的 schema 键：readOnly 键仍随值一起持久化，但不在此处渲染（一个语义只留一个写口） */
export function visibleProps(props: Record<string, JsonSchemaProp>): [string, JsonSchemaProp][] {
  return Object.entries(props).filter(([, p]) => !p.readOnly);
}

/** 默认值合并（纯函数，Vitest 覆盖）：存量配置优先，缺失键回填 schema 默认值 */
export function mergeDefaults(
  props: Record<string, JsonSchemaProp>,
  stored: Values | null | undefined,
): Values {
  const merged: Values = { ...stored };
  for (const [k, p] of Object.entries(props)) {
    if (merged[k] === undefined && p.default !== undefined) merged[k] = p.default;
  }
  return merged;
}

/** 整数控件值防御：Number() 永不返回 nullish，旧写法 `Number(x) ?? 0` 的 ?? 是死代码；
 *  undefined/NaN/非数值统一落 0 */
export function toFiniteNum(v: unknown): number {
  const n = Number(v);
  return Number.isFinite(n) ? n : 0;
}

/** string 键何时升级为下拉：必须给出**非空且全为非空串**的词表，否则退回自由文本 Input
 *  （空 enum 在 JSON Schema 里是"恒不合法"，不是"随便填"） */
export function enumChoices(prop: JsonSchemaProp): string[] | null {
  if (prop.type !== "string" || !Array.isArray(prop.enum)) return null;
  return prop.enum.length > 0 && prop.enum.every((v) => typeof v === "string" && v.length > 0)
    ? prop.enum
    : null;
}

export default function SchemaForm({ moduleId }: { moduleId: string }) {
  const styles = useStyles();
  const [schema, setSchema] = useState<Record<string, JsonSchemaProp> | null>(null);
  const [values, setValues] = useState<Values | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  // PERF-02：待提交键集合 + 防抖计时器 + 值快照（flush 在锁外读最新值）
  const dirtyKeys = useRef<Set<string>>(new Set());
  const timerRef = useRef<number | undefined>(undefined);
  const valuesRef = useRef<Values | null>(null);
  valuesRef.current = values;
  const schemaRef = useRef<Record<string, JsonSchemaProp> | null>(null);
  schemaRef.current = schema;

  const flush = useCallback(async () => {
    if (dirtyKeys.current.size === 0) return;
    dirtyKeys.current = new Set();
    const snapshot = valuesRef.current;
    if (!snapshot || !IN_TAURI) return;
    setSaving(true);
    try {
      await hostConfigSet(moduleId, snapshot);
      setError(null);
    } catch (e) {
      // 失败如实显错（顶部错误行）。不自动回滚输入值——校验类拒绝（如路径非法）
      // 下保留用户所打内容才能"改正即可重试"（T-B4-11 钉住的产品决策）
      setError(parseErr(e));
    } finally {
      setSaving(false);
    }
  }, [moduleId]);

  const scheduleSave = useCallback(
    (key: string, immediate: boolean) => {
      dirtyKeys.current.add(key);
      window.clearTimeout(timerRef.current);
      if (immediate) {
        void flush();
        return;
      }
      timerRef.current = window.setTimeout(() => void flush(), DEBOUNCE_MS);
    },
    [flush],
  );

  const save = (key: string, value: unknown, immediate = false) => {
    setValues((prev) => (prev ? { ...prev, [key]: value } : prev));
    // 同步推进快照：immediate flush 在 setState 渲染前执行，必须已含本次修改
    if (valuesRef.current) valuesRef.current = { ...valuesRef.current, [key]: value };
    scheduleSave(key, immediate);
  };

  useEffect(() => {
    if (!IN_TAURI) {
      setSchema({ max_entries: { type: "integer", title: "示例（浏览器预览无 IPC）" } });
      setValues({ max_entries: 5000 });
      return;
    }
    Promise.all([hostConfigSchema(moduleId), hostConfigGet<Values>(moduleId)])
      .then(([s, v]) => {
        const props = (s.properties ?? {}) as Record<string, JsonSchemaProp>;
        setSchema(props);
        // 空配置 → 填充 schema 默认值
        setValues(mergeDefaults(props, v));
      })
      .catch((e) => setError(String(e)));
  }, [moduleId]);

  // 卸载/切换模块前强制 flush：防抖中的最后一次输入不得丢失
  useEffect(
    () => () => {
      window.clearTimeout(timerRef.current);
      void flush();
    },
    [flush, moduleId],
  );

  if (!schema || !values) {
    return (
      <div className={styles.root}>
        <Text>{error ?? "加载配置中…"}</Text>
      </div>
    );
  }

  return (
    <div className={styles.root}>
      {error && <Text className={styles.err}>{error}</Text>}
      {saving && <Text className={styles.desc}>保存中…</Text>}
      {visibleProps(schema).map(([key, prop]) => {
        const choices = enumChoices(prop);
        return (
          <div className={styles.row} key={key}>
            <div className={styles.info}>
              <span className={styles.name}>{prop.title ?? key}</span>
              <span className={styles.desc}>{prop.description ?? ""}</span>
            </div>
            {prop.type === "boolean" && (
              <Switch
                checked={Boolean(values[key])}
                onChange={(_, d) => save(key, d.checked, true)}
              />
            )}
            {prop.type === "integer" && (
              <SpinButton
                value={toFiniteNum(values[key])}
                min={prop.minimum}
                max={prop.maximum}
                step={Math.max(1, Math.round(((prop.maximum ?? 100) - (prop.minimum ?? 0)) / 100))}
                onChange={(_, d) => {
                  save(key, d.value ?? toFiniteNum(values[key]));
                }}
                appearance="outline"
              />
            )}
            {choices && (
              <Dropdown
                size="small"
                style={{ minWidth: "240px" }}
                value={String(values[key] ?? "")}
                selectedOptions={[String(values[key] ?? "")]}
                onOptionSelect={(_, d) => save(key, d.optionValue, true)}
              >
                {choices.map((v) => (
                  <Option key={v} value={v} text={v}>
                    {v}
                  </Option>
                ))}
              </Dropdown>
            )}
            {prop.type === "string" && !choices && (
              <Input
                value={String(values[key] ?? "")}
                onChange={(_, d) => save(key, d.value)}
                style={{ width: "240px" }}
              />
            )}
            {prop.type === "array" && prop.items?.type === "string" && (
              <Textarea
                value={Array.isArray(values[key]) ? (values[key] as string[]).join(",") : ""}
                placeholder="逗号分隔"
                resize="vertical"
                style={{ width: "280px", minHeight: "40px" }}
                onChange={(_, d) =>
                  save(
                    key,
                    d.value
                      .split(/[,\n]/)
                      .map((s) => s.trim())
                      .filter(Boolean),
                  )
                }
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

function parseErr(e: unknown): string {
  const anyE = e as { data?: { message?: string; hint?: string } };
  if (anyE?.data?.message) return `${anyE.data.message} ${anyE.data.hint ?? ""}`;
  return String(e);
}
