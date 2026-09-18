import { describe, expect, it } from "vitest";
import { parseAppError } from "../client";

// 错误契约（M1 冻结）：{ kind, data: { code, message, hint?, retryable? } }
describe("parseAppError", () => {
  it("接受完整 AppErrorDto（含 hint/retryable）", () => {
    const e = { kind: "Storage", data: { code: "E042", message: "db 锁定", hint: "重试", retryable: true } };
    expect(parseAppError(e)).toBe(e);
  });

  it("最小合法形态：仅 kind + data.code 即通过", () => {
    expect(parseAppError({ kind: "Module", data: { code: "X" } })?.data.code).toBe("X");
  });

  it("拒绝：字符串（Tauri window 事件 / fetch 失败常见形态）", () => {
    expect(parseAppError("boom")).toBeNull();
  });

  it("拒绝：缺 kind", () => {
    expect(parseAppError({ data: { code: "X" } })).toBeNull();
  });

  it("拒绝：data 非对象或缺 code", () => {
    expect(parseAppError({ kind: "Module", data: { message: "no code" } })).toBeNull();
    expect(parseAppError({ kind: "Module", data: "str" })).toBeNull();
  });

  it("拒绝：null / undefined / 数字 / 数组", () => {
    expect(parseAppError(null)).toBeNull();
    expect(parseAppError(undefined)).toBeNull();
    expect(parseAppError(42)).toBeNull();
    expect(parseAppError([1, 2])).toBeNull();
  });
});
