import { describe, expect, it } from "vitest";

/**
 * T-B5-9（明示不做）的红线扫断（09 §10.2 本行回归列字面名）。
 *
 * 为什么"登记即验收"还需要一枚测：十档不做里最容易出的假不是代码缺失，而是**文案先行**——
 * 面板上先立一个禁用按钮或一句"支持合并"，功能却没有，用户按字面理解就错了。
 * 于是判据取可机检的两半：
 * ① 生产源码里**不存在**被裁掉那四档的实现面（三方合并 / 中继云中转 / 干跑预演 / 忽略模式）；
 * ② 面板上出现的每一枚 `DeferredBadge` 都必须带 `decisionRef`，且标签集**恰等于**
 *    本行登记的三档——多一枚（未经登记的"预告"）与少一枚（把已登记的诚实抹掉）同样判红。
 * 扫断先要证明"扫到东西"：正对照钉住三个读取口的调用点确实在被扫的文本里，
 * 否则"零命中"可能只是因为 glob 拼错、什么都没读。
 */

const sources = Object.entries(
  import.meta.glob("../*.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>,
).filter(([rel]) => !rel.includes("__tests__") && !/\.test\.tsx?$/.test(rel));

const BADGE = /<DeferredBadge\b[^>]*>/g;

describe("sync deferred items (T-B5-9)", () => {
  it("syncDeferredItems_registeredNotImplemented", () => {
    expect(sources.length, "glob 未读到同步模块生产源码").toBeGreaterThanOrEqual(6);
    const all = sources.map(([, src]) => src).join("\n");

    // 正对照：扫描面确实覆盖到本批读面调用点（缺一枚= glob 面不对，不是"没有实现"）
    for (const marker of ["syncConflictRestore", "syncDatasetsGet", "syncRunsGet", "syncSetPaused"])
      expect(all, `扫描面缺 ${marker}`).toContain(marker);

    // ① 四档不做：先摘掉延后徽标（徽标本身按设计提到这些词），剩下的正文不许有实现面
    const stripped = all.replace(BADGE, "");
    for (const forbidden of [
      "三方合并",
      "3-way",
      "threeWay",
      "mergeConflict",
      "中继",
      "云中转",
      "bisync",
      "预演",
      "干跑",
      "dryRun",
      "忽略模式",
      "ignorePattern",
    ])
      expect(stripped, `出现未实现的"${forbidden}"字样`).not.toContain(forbidden);

    // 也不许以禁用钮伪装：整个模块里 disabled 的按钮只有既有 busy/状态门（无 deferred 专用臂）
    expect(stripped).not.toMatch(/disabled[^>]*\{?[^\n]*(三方|中继|预演|忽略)/);

    // ② 徽标即诚实：每一枚都要有出处，且标签集与本行登记一一对应
    const badges = [...all.matchAll(BADGE)].map((m) => m[0]);
    expect(badges.length, "延后徽标一枚不剩＝把已登记的诚实抹掉了").toBeGreaterThanOrEqual(1);
    const labels: string[] = [];
    for (const b of badges) {
      const label = /label="([^"]+)"/.exec(b)?.[1] ?? "";
      const ref = /decisionRef="([^"]+)"/.exec(b)?.[1] ?? "";
      expect(label, `徽标缺 label：${b}`).not.toBe("");
      expect(ref, `徽标 ${label} 无出处＝文案先行`).toMatch(/^09 §10\.[23]/);
      labels.push(label);
    }
    expect(labels.sort()).toEqual(["三方合并", "云中转", "剪贴板数据集"].slice().sort());
  });
});
