import { describe, expect, it } from "vitest";

/**
 * T-B7-28（B7 批次尾 · §7.3-(b) 明示不做）的源码扫断，与 T-B5-9/T-B6-13/term 同谱双向钉：
 * 跨机"拖拽传文件"（把文件从桌面丢过共享边缘即传送）判归 B8 待裁决——需要宿主
 * shell 级落点，风险面大；按钮/对话框推送已在（承重⑤判定）。本域只挂徽标。
 * 判据两半：① kvm 生产源码零实现形状词（dragDrop / fileDrop，且"拖拽"字样只许
 * 出现在徽标 label 里——先剥徽标再扫）；② 徽标出处指向 §7.3 档号且标签集恰等。
 */

const sources = Object.entries({
  ...(import.meta.glob("../*.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>),
  ...(import.meta.glob("../*.ts", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>),
}).filter(([rel]) => !rel.includes("__tests__") && !/\.test\.tsx?$/.test(rel));

const BADGE = /<DeferredBadge\b[^>]*>/g;

describe("kvm deferred items (T-B7-28)", () => {
  it("kvmDeferredItems_registeredNotImplemented", () => {
    expect(sources.length, "glob 未读到 kvm 模块生产源码").toBeGreaterThanOrEqual(1);
    const all = sources.map(([, src]) => src).join("\n");

    // 正对照：扫描面覆盖到已交付的真调用点（推送通道在场＝徽标才不是凭空预告）
    for (const marker of ["kvmSendFile", "kvmSetEdgeMap", "kvmEdgeMap"])
      expect(all, `扫描面缺 ${marker}`).toContain(marker);

    // ① 延后档的实现形状词：剥掉徽标后零命中（"拖拽"是徽标 label 的字面，随剥除豁免）
    const stripped = all.replace(BADGE, "");
    for (const forbidden of [/dragDrop/i, /fileDrop/i, "拖拽"])
      expect(stripped, `出现未登记的实现形状"${forbidden}"`).not.toMatch(forbidden);

    // 禁用钮伪装
    expect(stripped).not.toMatch(/disabled[^>]*\{?[^\n]*拖拽/);

    // ② 徽标双向钉：出处 §7.3 档号，标签恰一枚
    const badges = [...all.matchAll(BADGE)].map((m) => m[0]);
    expect(badges.length, "延后徽标被抹掉＝诚实消失").toBe(1);
    const labels: string[] = [];
    for (const b of badges) {
      const label = /label="([^"]+)"/.exec(b)?.[1] ?? "";
      const ref = /decisionRef="([^"]+)"/.exec(b)?.[1] ?? "";
      expect(label, `徽标缺 label：${b}`).not.toBe("");
      expect(ref, `徽标 ${label} 无出处＝文案先行`).toMatch(/^09 §7\.3-\(/);
      labels.push(label);
    }
    expect(labels).toEqual(["拖拽传文件"]);
  });

  it("kvmPanel_dragBadge_verbatimLiteral", () => {
    const src = sources.find(([rel]) => rel.includes("KvmPanel"))?.[1];
    expect(src, "KvmPanel 未落进扫描面").toBeTruthy();
    expect(src).toContain('<DeferredBadge label="拖拽传文件" decisionRef="09 §7.3-(b)" />');
    for (const talk of ["即将上线", "敬请期待", "下个版本"])
      expect(src, `不许有画饼话术「${talk}」`).not.toContain(talk);
  });
});
