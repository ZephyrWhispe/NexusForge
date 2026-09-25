import { describe, expect, it } from "vitest";

/**
 * T-B7-28 曾为本域唯一延后项「拖拽传文件」（§7.3-(b)）挂徽标双向钉；
 * T-B8-1（D-33，2026-09-25 B8 裁决放行）交付该行——按 B5/B6/B7 同谱双向钉纪律把断言**翻正**：
 * ① 该档徽标数恰 0（无新登记凭空回挂＝红）；② 实现形状锚 onDragDropEvent / planDropSend /
 * DragDropSendDialog 反转为**必在场**（静默拔掉已交付能力＝红）；③ 画饼话术与 disabled
 * 伪装钮负例维持。"拖拽"字样随转正成为合法文案词（B7-25/27 禁词收缩先例第四条）。
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

describe("kvm deferred items (T-B7-28 → retired by T-B8-1)", () => {
  it("kvmDeferredItems_badgeRetiredWithDelivery", () => {
    expect(sources.length, "glob 未读到 kvm 模块生产源码").toBeGreaterThanOrEqual(3);
    const all = sources.map(([, src]) => src).join("\n");

    // ② 必在场锚：既有交付 + 本行转正的实现形状
    for (const marker of [
      "kvmSendFile",
      "kvmSetEdgeMap",
      "kvmEdgeMap",
      "onDragDropEvent",
      "planDropSend",
      "DragDropSendDialog",
    ])
      expect(all, `缺必在场锚 ${marker}`).toContain(marker);

    // ① 徽标恰 0（kvm 域唯一登记延后项已交付）
    const badges = [...all.matchAll(BADGE)];
    expect(badges.length, "kvm 域延后档已全部交付——再挂徽标须有新的 §7.3 登记并同步本断言").toBe(0);

    // ③ 画饼话术与伪装钮负例
    for (const talk of ["即将上线", "敬请期待", "下个版本"])
      expect(all, `不许有画饼话术「${talk}」`).not.toContain(talk);
    expect(all).not.toMatch(/disabled[^>]*\{?[^\n]*拖拽/);
  });

  it("kvmPanel_dragBadge_removedAndDialogMountedPerDrop", () => {
    const src = sources.find(([rel]) => rel.includes("KvmPanel"))?.[1];
    expect(src, "KvmPanel 未落进扫描面").toBeTruthy();
    expect(src).not.toContain("DeferredBadge");
    expect(src).toContain("dropFiles &&");
  });
});
