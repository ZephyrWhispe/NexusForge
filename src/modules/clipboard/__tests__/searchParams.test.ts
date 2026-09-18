import { describe, expect, it } from "vitest";
import { clipSearchParams } from "../ClipboardPanel";

// 剪贴板筛选/分页参数构造（审查 D-17 首批纯逻辑测试之一）
describe("clipSearchParams", () => {
  it("空搜索 → text 不参与过滤（undefined 而非空串）", () => {
    expect(clipSearchParams("", "all", 0).text).toBeUndefined();
  });

  it("非空搜索原样下传", () => {
    expect(clipSearchParams("github token", "all", 0).text).toBe("github token");
  });

  it('"all" 分组 → group 不参与过滤；具体分组下传', () => {
    expect(clipSearchParams("", "all", 0).group).toBeUndefined();
    expect(clipSearchParams("", "code", 0).group).toBe("code");
  });

  it("分页：size 默认 50，可覆写；page 原样", () => {
    const q = clipSearchParams("", "all", 3);
    expect(q).toEqual({ text: undefined, group: undefined, page: 3, size: 50 });
    expect(clipSearchParams("x", "url", 1, 20)).toEqual({ text: "x", group: "url", page: 1, size: 20 });
  });
});
