import { describe, expect, it } from "vitest";

import { isMonacoTeardownCanceled } from "../notifications";

// D-29 B1 批次尾实启冒烟修复的回归：monaco 销毁期内部 Delayer 批量 reject
// （~200 条 Canceled/3s）把宿主日志刷屏（hostLog 在去重前就会执行）。
// 过滤谓词必须「窄而准」：只吞 name=Canceled 且栈内含 monaco Delayer.cancel
// 的 upstream 噪声；任何一臂失配（含真实错误的同名字段）都必须照常上报，
// 不掩护真实错误 —— 负例臂是本测试的存在的意义。

const MONACO_STACK = [
  "Error: Canceled",
  "    at Delayer.cancel (monaco.editor.js:21:12345)",
  "    at t.dispose (monaco.editor.js:33:678)",
].join("\n");

describe("isMonacoTeardownCanceled（全局 rejection 过滤谓词）", () => {
  it("matchesMonacoDelayerCanceled_onlyExactShape：正例臂", () => {
    expect(isMonacoTeardownCanceled({ name: "Canceled", stack: MONACO_STACK })).toBe(true);
    // 真实 Error 实例（同 name/stack）同样命中——谓词不依赖对象来源
    const err = new Error("Canceled");
    err.name = "Canceled";
    err.stack = MONACO_STACK;
    expect(isMonacoTeardownCanceled(err)).toBe(true);
  });

  it("everyNearMissStillReports：负例臂（失配一律上报，不掩护真实错误）", () => {
    // Canceled 但无栈 / 栈无 Delayer.cancel（例如业务侧自造 Canceled）
    expect(isMonacoTeardownCanceled({ name: "Canceled" })).toBe(false);
    expect(isMonacoTeardownCanceled({ name: "Canceled", stack: "Error\n at somewhere.Else" })).toBe(false);
    // 同栈但 name 不同（真实错误恰好穿过 monaco 栈帧）
    expect(isMonacoTeardownCanceled({ name: "TypeError", stack: MONACO_STACK })).toBe(false);
    // 原始值 / nullish 输入不炸谓词
    expect(isMonacoTeardownCanceled(null)).toBe(false);
    expect(isMonacoTeardownCanceled(undefined)).toBe(false);
    expect(isMonacoTeardownCanceled("Canceled")).toBe(false);
  });
});
