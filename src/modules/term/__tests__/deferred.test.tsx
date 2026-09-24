import { describe, expect, it } from "vitest";

/**
 * T-B7-28（B7 批次尾 · §7.3-a 明示不做）的源码扫断，与 T-B5-9/T-B6-13 同谱双向钉：
 * 交付=登记而非实现。SSH 连接池统一（term per-tab 长会话 × file per-op 短连接合一）
 * 判归 B8 待裁决（09 §7.3-(a)），本域只挂徽标。判据两半：
 * ① term 生产源码里不存在连接池统一体的实现形状词（connectionPool / mux）；
 * ② 出现的每一枚 DeferredBadge 都带 §7.3 出处，且标签集恰等于登记一枚——
 *    多一枚（未登记预告）与少一枚（抹掉诚实）同样判红。
 * 正对照先钉扫描面本体（本批真交付的调用点在场），防"零命中其实啥也没扫"。
 * 禁词按实现形状取词、先剥徽标再扫（B5 教训：单字/泛词误伤合法文案）。
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

describe("term deferred items (T-B7-28)", () => {
  it("termDeferredItems_registeredNotImplemented", () => {
    expect(sources.length, "glob 未读到 term 模块生产源码").toBeGreaterThanOrEqual(2);
    const all = sources.map(([, src]) => src).join("\n");

    // 正对照：扫描面覆盖到 B7 term 链真交付的调用点（缺＝ glob 面不对，不是"没实现"）
    for (const marker of [
      "termSshConnect",
      "termSshFingerprintAck",
      "termSshExec",
      "termSftpList",
      "fileRemoteChmod",
    ])
      expect(all, `扫描面缺 ${marker}`).toContain(marker);

    // ① 延后档的实现形状词：剥掉徽标后零命中
    const stripped = all.replace(BADGE, "");
    for (const forbidden of [/connectionPool/i, /\bmux\b/i])
      expect(stripped, `出现未登记的实现形状"${forbidden}"`).not.toMatch(forbidden);

    // 禁用钮伪装：徽标语义不许长禁用按钮的皮（既有 busy 门不在此列）
    expect(stripped).not.toMatch(/disabled[^>]*\{?[^\n]*连接池/);

    // ② 徽标双向钉：出处指向 §7.3 档号（批次号会过期成死引用），标签恰一枚
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
    expect(labels).toEqual(["SSH 连接池统一"]);
  });

  it("termPanel_connectionPoolBadge_verbatimLiteral", () => {
    // 徽标逐字钉在面板工具条（实启可读属批次尾人工冒烟，此处钉 DOM 前的源形状）
    const src = sources.find(([rel]) => rel.includes("TerminalPanel"))?.[1];
    expect(src, "TerminalPanel 未落进扫描面").toBeTruthy();
    expect(src).toContain('<DeferredBadge label="SSH 连接池统一" decisionRef="09 §7.3-(a)" />');
    for (const talk of ["即将上线", "敬请期待", "下个版本"])
      expect(src, `不许有画饼话术「${talk}」`).not.toContain(talk);
  });
});
