import { describe, expect, it } from "vitest";

/**
 * T-B6-13（批次尾 · 明示不做）的源码扫断（09 §6.2 本行回归列字面名）。
 *
 * 与 T-B5-9 同谱的双向钉：交付=登记而非实现——最容易出的假不是代码缺失，而是
 * **文案先行**（面板立一个禁用钮或一句"支持镜像"，功能却没有）。判据两半：
 * ① 生产源码里不存在被裁那几档的实现形状词（rclone 驱动 / 挂载 / treemap /
 *    权限位 / diff 镜像 / 远端同步专用臂 / 回收站找回视图 / presign / 配额 / 网盘）；
 * ② 出现的每一枚 DeferredBadge 都带 §6.3 出处，且标签集**恰等于**登记三枚——
 *    多一枚（未登记预告）与少一枚（抹掉诚实）同样判红。
 * 正对照先钉扫描面本体（三枚读取口调用点在场），防"零命中其实啥也没扫"。
 * 禁词按**实现形状**取词（B5 教训：单字词会误伤合法文案），且先剥掉徽标再扫。
 */

// file 域的接线不只在 .tsx：runConnect 住 connectFlow.ts、魔术栏算式住
// magicTarget.ts——扫描面须含 .ts，否则"承诺词有命令"的交叉断言会瞎在 .ts 上。
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

describe("file deferred items (T-B6-13)", () => {
  it("fileDeferredItems_registeredNotImplemented", () => {
    expect(sources.length, "glob 未读到 file 模块生产源码").toBeGreaterThanOrEqual(5);
    const all = sources.map(([, src]) => src).join("\n");

    // 正对照：扫描面覆盖到本批真交付的调用点（缺＝ glob 面不对，不是"没实现"）
    for (const marker of ["fileRemoteDrivers", "fileRemoteConnect", "fileRemoteFingerprintAck"])
      expect(all, `扫描面缺 ${marker}`).toContain(marker);

    // ① 延后档的实现形状词：剥掉徽标（徽标按设计提到这些词）后零命中
    // （T-B7-25 摘除 "chmod"/"permission"：权限位面已真交付，禁词表随实装收缩——
    //   留着就是对已兑现承诺的假报警。
    //  T-B7-27 摘除 "netdisk"：词转正为七档路由 id（FilePanel 分派字面 `sub === "netdisk"`），
    //   网盘的实现形状仍由 rclone/mount/quota_used 三词钉住——摘的是路由名不是诚实面。）
    const stripped = all.replace(BADGE, "");
    for (const forbidden of [
      "rclone",
      "mount",
      "treemap",
      "mirror",
      "diff",
      "sftp-sync",
      "recycleView",
      "presign",
      "quota_used",
    ])
      expect(stripped, `出现未登记的实现形状"${forbidden}"`).not.toContain(forbidden);

    // 禁用钮伪装：deferred 语义不许长禁用按钮的皮（既有 busy 门不在此列）
    expect(stripped).not.toMatch(/disabled[^>]*\{?[^\n]*(网盘|镜像|treemap|回收站|权限)/);

    // ② 徽标双向钉：出处指向 §6.3 档号（B5 教训：批次号会过期成死引用），标签恰三枚
    const badges = [...all.matchAll(BADGE)].map((m) => m[0]);
    expect(badges.length, "延后徽标被抹掉＝诚实消失").toBeGreaterThanOrEqual(3);
    const labels: string[] = [];
    for (const b of badges) {
      const label = /label="([^"]+)"/.exec(b)?.[1] ?? "";
      const ref = /decisionRef="([^"]+)"/.exec(b)?.[1] ?? "";
      expect(label, `徽标缺 label：${b}`).not.toBe("");
      expect(ref, `徽标 ${label} 无出处＝文案先行`).toMatch(/^09 §6\.3-\(/);
      labels.push(label);
    }
    expect(labels.sort()).toEqual(
      ["网盘", "diff/镜像工作台", "treemap/回收站找回"].slice().sort(),
    );
  });

  it("netdiskSection_badgeOnlyNoDisabledButtons：网盘档只有徽标+诚实文案，零按钮形态伪装（T-B7-27 deferred 纪律镜像）", () => {
    const src = sources.find(([rel]) => rel.includes("NetdiskSection"))?.[1];
    expect(src, "NetdiskSection 未落进扫描面（glob 面不对＝正对照失效）").toBeTruthy();
    // 无按钮即无"禁用的就绪"；连 disabled 属性都不许出现
    expect(src).not.toContain("<Button");
    expect(src).not.toMatch(/disabled=\{|\bdisabled\b/);
    // 徽标逐字保留 T-B6-13 形状（挪档不重写：label/decisionRef 一字未动）
    expect(src).toContain('<DeferredBadge label="网盘" decisionRef="09 §6.3-(c)" />');
    for (const talk of ["即将上线", "敬请期待", "下个版本"])
      expect(src, `不许有画饼话术「${talk}」`).not.toContain(talk);
  });

  it("panels04_promised_words_match_wired_commands", () => {
    // panels/04 §6 验收点"文案承诺词表 × 命令接线表交叉断言"——防"副标题承诺
    // 搜索而 UI 不存在"这一族复发：每个上屏的承诺词都必须指得动一枚真命令。
    const all = sources.map(([, src]) => src).join("\n");
    const PROMISES: Array<[string, string]> = [
      ["搜索文件名", "fileSearch"],
      ["批量重命名", "fileRenamePlan"],
      ["压缩为 zip", "fileEnqueue"],
      ["目标目录（复制/移动用）", "fileEnqueue"],
      ["远程连接", "fileRemoteProfiles"],
      ["测试连接", "fileRemoteConnect"],
      ["指纹", "fileRemoteFingerprintAck"],
      ["下载", "fileEnqueue"],
      ["上传到此目录", "fileEnqueue"],
      ["断点文件", "fileOpsPending"],
    ];
    const seen = sources.map(([, src]) => src).join("\n");
    for (const [word, command] of PROMISES) {
      expect(seen, `承诺词「${word}」上屏了`).toContain(word);
      expect(all, `但命令 ${command} 不在接线表里（文案先行）`).toContain(command);
    }
    // 负例：徽标词不许同时是"承诺"——剥掉徽标后正文零画饼话术（上一测已钉
    // 实现形状词，这里钉的是话术：没有"网盘即将上线"式的预告文案）
    const stripped = all.replace(BADGE, "");
    for (const talk of ["即将上线", "敬请期待", "下个版本"]) {
      expect(stripped, `不许有画饼话术「${talk}」`).not.toContain(talk);
    }
  });
});
