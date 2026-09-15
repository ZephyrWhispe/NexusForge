import { useEffect, useState } from "react";
import {
  makeStyles,
  tokens,
  Switch,
  SpinButton,
  Input,
  Textarea,
  Text,
} from "@fluentui/react-components";
import { hostConfigGet, hostConfigSchema, hostConfigSet } from "../ipc/client";
import { IN_TAURI } from "../ipc/env";

/**
 * SchemaForm（docs/UI-PLAN.md U6-1/U6-3）：
 * 模块 config_schema（JSON Schema）→ Fluent 控件自动渲染；
 * 修改即校验即保存（Rust 侧 schema 校验失败回显错误）。
 * 支持：boolean→Switch；integer(min/max)→SpinButton；string→Input；array(string)→Textarea(逗号分隔)。
 */
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
}

type Values = Record<string, unknown>;

export default function SchemaForm({ moduleId }: { moduleId: string }) {
  const styles = useStyles();
  const [schema, setSchema] = useState<Record<string, JsonSchemaProp> | null>(null);
  const [values, setValues] = useState<Values | null>(null);
  const [error, setError] = useState<string | null>(null);

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
        const merged: Values = { ...v };
        for (const [k, p] of Object.entries(props)) {
          if (merged[k] === undefined && p.default !== undefined) merged[k] = p.default;
        }
        setValues(merged);
      })
      .catch((e) => setError(String(e)));
  }, [moduleId]);

  const save = (next: Values) => {
    setValues(next);
    if (!IN_TAURI) return;
    hostConfigSet(moduleId, next)
      .then(() => setError(null))
      .catch((e) => setError(parseErr(e)));
  };

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
      {Object.entries(schema).map(([key, prop]) => (
        <div className={styles.row} key={key}>
          <div className={styles.info}>
            <span className={styles.name}>{prop.title ?? key}</span>
            <span className={styles.desc}>{prop.description ?? ""}</span>
          </div>
          {prop.type === "boolean" && (
            <Switch
              checked={Boolean(values[key])}
              onChange={(_, d) => save({ ...values, [key]: d.checked })}
            />
          )}
          {prop.type === "integer" && (
            <SpinButton
              value={Number(values[key]) ?? 0}
              min={prop.minimum}
              max={prop.maximum}
              step={Math.max(1, Math.round(((prop.maximum ?? 100) - (prop.minimum ?? 0)) / 100))}
              onChange={(_, d) => {
                const v = d.value ?? Number(values[key]) ?? 0;
                save({ ...values, [key]: v });
              }}
              appearance="outline"
            />
          )}
          {prop.type === "string" && (
            <Input
              value={String(values[key] ?? "")}
              onChange={(_, d) => save({ ...values, [key]: d.value })}
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
                save({
                  ...values,
                  [key]: d.value
                    .split(/[,\n]/)
                    .map((s) => s.trim())
                    .filter(Boolean),
                })
              }
            />
          )}
        </div>
      ))}
    </div>
  );
}

function parseErr(e: unknown): string {
  const anyE = e as { data?: { message?: string; hint?: string } };
  if (anyE?.data?.message) return `${anyE.data.message} ${anyE.data.hint ?? ""}`;
  return String(e);
}
