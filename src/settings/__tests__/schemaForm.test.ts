import { describe, expect, it } from "vitest";
import { mergeDefaults, toFiniteNum } from "../SchemaForm";

// SchemaForm 默认值合并（审查 D-17 首批纯逻辑测试之一）
const schema = {
  max_entries: { type: "integer" as const, default: 5000 },
  hotkey: { type: "string" as const, default: "Ctrl+V" },
  enable_cloud: { type: "boolean" as const }, // 无 default
  retention_days: { type: "integer" as const, default: 30 },
};

describe("mergeDefaults", () => {
  it("空存量 → 全部回填 schema 默认值", () => {
    expect(mergeDefaults(schema, {})).toEqual({ max_entries: 5000, hotkey: "Ctrl+V", retention_days: 30 });
  });

  it("存量值优先，不被默认值覆盖（含 0 / false / 空串等假值）", () => {
    const stored = { max_entries: 0, hotkey: "" };
    expect(mergeDefaults(schema, stored)).toEqual({ max_entries: 0, hotkey: "", retention_days: 30 });
  });

  it("显式 null 是有意义存量值，不回填", () => {
    expect(mergeDefaults(schema, { hotkey: null })).toEqual({
      max_entries: 5000,
      hotkey: null,
      retention_days: 30,
    });
  });

  it("无 default 的键不出现在结果中", () => {
    expect(mergeDefaults(schema, {})).not.toHaveProperty("enable_cloud");
  });

  it("存量多出的未知键原样保留（后端负责裁剪）", () => {
    expect(mergeDefaults(schema, { legacy: 1 })).toMatchObject({ legacy: 1, max_entries: 5000 });
  });

  it("stored 为 null/undefined 不抛（首启 host_config_get 可能回 null）", () => {
    expect(mergeDefaults(schema, null)).toEqual({ max_entries: 5000, hotkey: "Ctrl+V", retention_days: 30 });
    expect(mergeDefaults(schema, undefined)).toEqual({ max_entries: 5000, hotkey: "Ctrl+V", retention_days: 30 });
  });
});

describe("toFiniteNum", () => {
  it("undefined/NaN/非数值字符串一律落 0（旧写法 Number(x) ?? 0 会漏 NaN）", () => {
    expect(toFiniteNum(undefined)).toBe(0);
    expect(toFiniteNum(NaN)).toBe(0);
    expect(toFiniteNum("abc")).toBe(0);
    expect(toFiniteNum(null)).toBe(0); // Number(null) === 0，本就是 0
  });

  it("数值与数值字符串正常转换", () => {
    expect(toFiniteNum(42)).toBe(42);
    expect(toFiniteNum("3.7")).toBe(3.7);
  });

  it("Infinity 视为非法落 0", () => {
    expect(toFiniteNum(Infinity)).toBe(0);
  });
});
