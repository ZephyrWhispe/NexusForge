import { describe, expect, it } from "vitest";

import { MODULES, SUBNAV, isModuleId, type ModuleId } from "../modules";
import { PANELS, missingPanelKeys } from "../panels";
// vite ?raw：对构建器眼见的源文件本体做静态断言（同 indexHtmlInlineStyle 惯例）
import mainWorkbenchSrc from "../../windows/MainWorkbench.tsx?raw";

// D-29 B0/T-B0-1+6（docs/impl/09 §3.1）：注册表穷尽性 = 兜底三元与"模块界面待实现"
// 静默占位的结构性解毒剂。正例钉当前全绿，负例证明断言本身可红。

describe("PANELS 注册表穷尽性（T-B0-1）", () => {
  it("panelsRegistry_coversEveryModuleId：每个模块 id 都有面板与副标题", () => {
    expect(missingPanelKeys(PANELS)).toEqual([]);
    for (const m of MODULES) {
      const def = PANELS[m.id];
      expect(def, `${m.id} 缺面板`).toBeDefined();
      expect(def.subtitle.trim().length, `${m.id} 副标题为空`).toBeGreaterThan(0);
      expect(typeof def.panel, `${m.id} panel 非组件`).not.toBe("undefined");
    }
    expect(Object.keys(PANELS)).toHaveLength(MODULES.length);
  });

  it("panelsRegistry_missingKey_selfCheckTurnsRed：删键必被检出（断言非恒真）", () => {
    const broken = { ...PANELS } as Partial<Record<ModuleId, (typeof PANELS)[ModuleId]>>;
    delete broken.ocr;
    delete broken.sync;
    expect(missingPanelKeys(broken)).toEqual(["ocr", "sync"]);
    // 假 id 不混入判据（MODULES 是唯一事实源）
    expect(missingPanelKeys({ bogus: PANELS.ocr } as never)).toEqual(MODULES.map((m) => m.id));
  });

  it("mainWorkbench_noFallbackLiteral：工作台源码无兜底三元残留", () => {
    for (const banned of ["模块界面待实现", "isKvm ?", "isSync ?", "将在对应阶段交付"]) {
      expect(mainWorkbenchSrc, `MainWorkbench 仍含 ${banned}`).not.toContain(banned);
    }
    // 阳性对照：证明确实在断言真源码而非空串
    expect(mainWorkbenchSrc).toContain("PANELS[moduleId]");
  });
});

describe("SUBNAV 注册表（T-B0-6）", () => {
  it("subnavRegistry_coversAllModules：14 模块每个都有分组表（可为空数组）", () => {
    const ids = Object.keys(SUBNAV);
    expect(ids.sort()).toEqual(MODULES.map((m) => m.id).sort());
    for (const id of ids) {
      expect(Array.isArray(SUBNAV[id as ModuleId]), `${id} 分组表非数组`).toBe(true);
    }
    expect(isModuleId("clipboard")).toBe(true);
    expect(isModuleId("__settings")).toBe(false);
    expect(isModuleId("bogus")).toBe(false);
  });

  it("subnav_clipboardGroupsMigrated_countsPreserved：剪切板原 6 分组逐项不丢", () => {
    // T-B3-1 双维度：原单 section「剪切板」的六项整体迁进新 section「筛选」（新增「视图」section），
    // 逐项 id/label/badgeKey 判据一字未改——迁移证明仍成立；section 数由 1 变 2 属本行 IA 演进。
    const sections = SUBNAV.clipboard;
    expect(sections.map((s) => s.group)).toEqual(["视图", "筛选"]);
    const group = sections[1];
    expect(group.items.map((i) => i.id)).toEqual(["all", "text", "code", "url", "secret", "files"]);
    expect(group.items.map((i) => i.label)).toEqual(["全部", "文本", "代码", "链接", "敏感", "文件"]);
    // 计数徽标键与旧 CLIP_GROUPS 的 id 一一对应（badgeKey 即 counts 索引）
    expect(group.items.map((i) => i.badgeKey)).toEqual(group.items.map((i) => i.id));
  });

  it("subnavRouting_noModuleTernaryInWorkbench：工作台不再按模块硬分叉选择态", () => {
    const src = String(mainWorkbenchSrc);
    // 负例：原 `moduleId === "proxy" ? proxySub : group` 一类的模块三元清零
    expect(src).not.toContain('moduleId === "proxy" ?');
    expect(src).not.toContain("subActive");
    expect(src).not.toContain("subSelect=");
    // 正对照防空洞：改由 modules.ts 路由表单点派发（读写各经纯函数）
    expect(src).toContain("subnavActive(");
    expect(src).toContain("subnavSelect(");
    expect(src).toContain("subnavSetters[sel.key]");
  });
});
