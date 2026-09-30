import { describe, expect, it } from "vitest";

import { MODULES, SUBNAV, type ModuleId } from "../modules";

/**
 * D-42 ③：布局结构合规门禁（棘轮台账）。
 *
 * 病因不是"缺规范"而是"规范从未落到运行时"——docs/panels/2026-09-19/00-ui-layout-spec.md
 * 写了骨架高度、控件宽度档、单滚动容器与禁原生控件，而 15 份面板档各自手摆，
 * 组件族（PanelHeader/DataToolbar/SwitchSetting/PathPicker）又从未进过 09 的 B0 任务表。
 * 本批只交付组件族＋壳，14 个面板本体不重排，于是把"当前欠债"逐文件记账，
 * 让此后每一枚原生控件、每一处写死宽度、每一个第二滚动容器都必须先过台账：
 *  · 增长守卫——实得 > 台账即红（新增漂移无处藏）；
 *  · 反向过期守卫——实得 < 台账同样判红（逼面板批把条目改小或删除，欠债只减不增）；
 *  与 STD-10 的 $TopicReserve 台账同谱（显式债＋忘摘即红）。
 *
 * 扫描面＝面板/设置本体（src/modules/**\/*.tsx ＋ src/settings/*.tsx，排除 __tests__）；
 * src/components 与 src/layout 豁免——尺寸档位真源 nfTiers.ts 自己必含 120px/48px 等字面。
 * 选 vitest 源扫而非 tools/assert-patterns.ps1：后者三条扫全是 Rust 形状（顶格 #[cfg(test)]
 * 截断、±6 行锚点窗、BOM 维护），表达不了"每文件配额计数"，且只在被调用时跑；
 * 本档天然进每提交必跑的 npm test。唯一例外＝global.css 的下发侧：vitest 读不到 .css 原文，
 * 该判据归 assert-patterns 的 D-42 扫描（按"哪一侧读得到"分面，不为一处判据引入 @types/node）。
 */

type Dim = "nativeControls" | "inlineWidths" | "scrollY";

/** 台账起点＝D-42 壳批落地后实测（面板 45 个 tsx：原生控件 14／写死宽度 22／≥2 滚动容器 10 文件）。
 *  D-43 C4 摘除 VaultPanel 整行（原生控件 1＝生成器 number 微调钮换 SpinButton、写死宽度 4＝
 *  全走 TIER_W/FORM_CARD_W、主体单滚动容器 scrollY 1 枚不构成违规）；
 *  C5 摘除 ForwardSection 整行（两枚原生 checkbox → Fluent Checkbox）、TerminalPanel scrollY 3→2
 *  （会话列表/SSH 表单的 maxHeight 内层视口撤除，余下第二枚是 Docker 日志输出面——
 *  面板档 08-term §7.1 的有意例外，日志随 tail 增长，无自留视口会把整页撑长）；
 *  C6 摘除 SysPanel 整行（三枚内层视口收敛为主体单滚动＋分页页脚，scrollY 3→1 不再构成违规）。
 *  余下欠债逐行随 C7–C8 下调。 */
const LEDGER: { file: string; counts: Record<Dim, number> }[] = [
  { file: "src/modules/automation/RulesPanel.tsx", counts: { nativeControls: 4, inlineWidths: 0, scrollY: 2 } },
  { file: "src/modules/clipboard/panels/HistorySection.tsx", counts: { nativeControls: 0, inlineWidths: 1, scrollY: 2 } },
  { file: "src/modules/clipboard/panels/SecretSection.tsx", counts: { nativeControls: 0, inlineWidths: 0, scrollY: 2 } },
  { file: "src/modules/clipboard/panels/StackSection.tsx", counts: { nativeControls: 1, inlineWidths: 3, scrollY: 0 } },
  { file: "src/modules/desktop/DesktopPanel.tsx", counts: { nativeControls: 0, inlineWidths: 1, scrollY: 0 } },
  { file: "src/modules/editor/EditorPanel.tsx", counts: { nativeControls: 0, inlineWidths: 0, scrollY: 2 } },
  { file: "src/modules/file/BatchSection.tsx", counts: { nativeControls: 1, inlineWidths: 0, scrollY: 0 } },
  { file: "src/modules/file/ConnectionsSection.tsx", counts: { nativeControls: 1, inlineWidths: 2, scrollY: 2 } },
  { file: "src/modules/file/FilePanel.tsx", counts: { nativeControls: 0, inlineWidths: 1, scrollY: 2 } },
  { file: "src/modules/notes/NotesPanel.tsx", counts: { nativeControls: 1, inlineWidths: 0, scrollY: 4 } },
  { file: "src/modules/proxy/panels/KernelSection.tsx", counts: { nativeControls: 0, inlineWidths: 1, scrollY: 0 } },
  { file: "src/modules/proxy/panels/RulesSection.tsx", counts: { nativeControls: 3, inlineWidths: 0, scrollY: 0 } },
  { file: "src/modules/screenshot/BeautifyPopover.tsx", counts: { nativeControls: 0, inlineWidths: 4, scrollY: 0 } },
  { file: "src/modules/screenshot/ScreenshotPanel.tsx", counts: { nativeControls: 0, inlineWidths: 3, scrollY: 0 } },
  { file: "src/modules/sync/ConflictsSection.tsx", counts: { nativeControls: 0, inlineWidths: 0, scrollY: 2 } },
  { file: "src/modules/term/TerminalPanel.tsx", counts: { nativeControls: 0, inlineWidths: 0, scrollY: 2 } },
  { file: "src/settings/SchemaForm.tsx", counts: { nativeControls: 0, inlineWidths: 2, scrollY: 0 } },
];

/**
 * 空 SUBNAV 的待拆台账（起点 10 枚，D-43 C4 起逐枚摘除，现 7 枚；00 规范 1 节：面板必须有二级导航槽位，
 * 否则一屏塞满纵向滚动＝"元素排列粗暴"的形态学根因）。until 指向各面板档，
 * 该面板批落地子导航注册后必须删掉对应条目，否则 subnav_registration_or_ledger 判红。
 */
const PENDING_SUBNAV: { id: ModuleId; until: string }[] = [
  { id: "screenshot", until: "面板批·docs/panels/2026-09-19/09-screenshot.md" },
  { id: "ocr", until: "面板批·docs/panels/2026-09-19/10-ocr.md" },
  { id: "desktop", until: "面板批·docs/panels/2026-09-19/05-desktop.md" },
  { id: "kvm", until: "面板批·docs/panels/2026-09-19/06-kvm.md" },
  { id: "editor", until: "面板批·docs/panels/2026-09-19/11-editor.md" },
  { id: "notes", until: "面板批·docs/panels/2026-09-19/12-notes.md" },
  { id: "automation", until: "面板批·docs/panels/2026-09-19/13-automation.md" },
];

/** 0 节：原生控件＝裸 select/textarea/checkbox 与浏览器 number 微调钮（Fluent Input 透传 type 同样命中） */
const NATIVE_PATTERNS = [
  "<select[\\s>]",
  "<textarea[\\s>]",
  '<input\\b[^>]*?type="checkbox"',
  '\\btype="number"',
];
/** 3 节：控件宽度一律走 nfTiers.TIER_W，写死 px 即欠债 */
const WIDTH_PATTERN = 'width:\\s*"\\d+px"';
/** 1 节：单滚动容器——第 2 枚 overflowY 起计违规（第 1 枚是面板自己的主体） */
const SCROLL_PATTERN = 'overflowY:\\s*"(?:auto|scroll)"';

const count = (src: string, pattern: string) => (src.match(new RegExp(pattern, "g")) ?? []).length;

const raw = {
  ...(import.meta.glob("/src/modules/**/*.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>),
  ...(import.meta.glob("/src/settings/*.tsx", {
    query: "?raw",
    import: "default",
    eager: true,
  }) as Record<string, string>),
};

/** 壳层形状钉另取两枚具名 tsx（不在面板台账扫描面内） */
const shellRaw = import.meta.glob(
  ["/src/windows/MainWorkbench.tsx", "/src/layout/SubNav.tsx"],
  { query: "?raw", import: "default", eager: true },
) as Record<string, string>;

/** 窄栏下发用的四枚自定义属性名（另一半"global.css 是否真的下发"由 tools/assert-patterns.ps1 的
 *  D-42 扫描钉：vitest 下 .css 的 import.meta.glob 三条通路（?raw / ?inline / ?raw&inline）实测全返回
 *  空串且 build.assetsInlineLimit:0 对 test 模式零影响，源扫读不到 CSS 原文，故按可读性分面而非硬凑 node:fs） */
const SUBNAV_VARS = [
  "--nf-subnav-w",
  "--nf-subnav-pad",
  "--nf-subnav-display",
  "--nf-subnav-group-display",
];

/** 键归一为仓库相对路径（去 glob 的前导 "/"），并排除测试夹具自身 */
const sources: [string, string][] = Object.entries(raw)
  .filter(([p]) => !p.includes("__tests__"))
  .map(([p, src]) => [p.replace(/^\//, ""), src] as [string, string])
  .sort((a, b) => (a[0] < b[0] ? -1 : 1));

const measure = (src: string, dim: Dim): number => {
  if (dim === "nativeControls")
    return NATIVE_PATTERNS.reduce((acc, p) => acc + count(src, p), 0);
  if (dim === "inlineWidths") return count(src, WIDTH_PATTERN);
  return count(src, SCROLL_PATTERN);
};

/** 各维度的"违规"判据：scrollY 只有第 2 枚起才算双层滚动，其余两维一命中即欠债 */
const violates = (dim: Dim, n: number) => (dim === "scrollY" ? n >= 2 : n > 0);

/** 具名探针：正对照地板的两条腿——四枚本批在排的面板＋settings glob 的代表（两枚 glob 任一脱靶即红） */
const NAMED_PROBES = [
  "src/modules/term/TerminalPanel.tsx",
  "src/modules/sys/SysPanel.tsx",
  "src/modules/vault/VaultPanel.tsx",
  "src/modules/notes/NotesPanel.tsx",
  "src/settings/SchemaForm.tsx",
];

/** 台账合计只累加"构成违规"的条目，与 ratchet 的口径一致（scrollY=1 的主体滚窗不计入欠债） */
const bookedTotal = (dim: Dim) =>
  LEDGER.reduce((acc, e) => acc + (violates(dim, e.counts[dim]) ? e.counts[dim] : 0), 0);

const bookedMultiScrollFiles = LEDGER.filter((e) => violates("scrollY", e.counts.scrollY)).length;

function ratchet(dim: Dim): { problems: string[]; total: number } {
  const byFile = new Map(LEDGER.map((e) => [e.file, e.counts[dim]]));
  const known = new Set(LEDGER.map((e) => e.file));
  const problems: string[] = [];
  let total = 0;

  for (const [file, src] of sources) {
    const n = measure(src, dim);
    if (!violates(dim, n)) continue;
    total += n;
    if (!known.has(file)) {
      problems.push(`${file}：${dim}=${n} 未登记（新增漂移——按规范收敛，或补台账并写明所属批次）`);
      continue;
    }
    const booked = byFile.get(file) ?? 0;
    if (n > booked) problems.push(`${file}：${dim} 由 ${booked} 增至 ${n}（棘轮只减不增）`);
    if (n < booked) problems.push(`${file}：${dim} 已由 ${booked} 降至 ${n}——台账过期，请把条目改小或删除`);
  }

  for (const e of LEDGER) {
    const booked = e.counts[dim];
    if (!violates(dim, booked)) continue;
    if (!sources.some(([f]) => f === e.file))
      problems.push(`台账文件 ${e.file} 不在扫描面内（重命名/删除后请同步台账）`);
  }
  return { problems, total };
}

const dims: Dim[] = ["nativeControls", "inlineWidths", "scrollY"];

describe("D-42 layout compliance ratchet", () => {
  it("positive_control_floors_are_ledger_derived", () => {
    // 正对照地板（D-43 ③改台账派生）：原口径钉的是 14/22/10/20/10 五枚绝对数，
    // 那是"当前欠债"的快照——面板批把欠债真降下来时反而会判红（本批做成即红），
    // 故下限一律改为"实测 ≥ 台账合计"：RHS 手工记账、LHS 实测，非恒等式，
    // glob 脱靶时 LHS 归零即红；具名探针文件在场是同时归零盲区的唯一兜底，不可摘。
    expect(sources.length, "扫描面文件数").toBeGreaterThanOrEqual(40);
    for (const f of NAMED_PROBES)
      expect(sources.some(([p]) => p === f), `扫描面缺具名探针 ${f}`).toBe(true);

    const totals = Object.fromEntries(
      dims.map((d) => [d, sources.reduce((acc, [, src]) => acc + measure(src, d), 0)]),
    ) as Record<Dim, number>;
    for (const d of dims) {
      expect(totals[d], `${d} 实测低于台账合计（扫描面脱靶或台账漂移）`).toBeGreaterThanOrEqual(
        bookedTotal(d),
      );
    }
    expect(
      sources.filter(([, src]) => violates("scrollY", measure(src, "scrollY"))).length,
      "≥2 滚动容器的文件数低于台账",
    ).toBeGreaterThanOrEqual(bookedMultiScrollFiles);

    // SUBNAV 注册面的地板：防"把已注册子导航连同 PENDING 条目一起删掉"的双绿盲区
    expect(MODULES.filter((m) => SUBNAV[m.id].length > 0).length, "已注册 SUBNAV 的模块数").toBeGreaterThanOrEqual(4);
  });

  it("no_new_native_controls", () => {
    expect(ratchet("nativeControls").problems).toEqual([]);
  });

  it("no_new_inline_widths", () => {
    expect(ratchet("inlineWidths").problems).toEqual([]);
  });

  it("single_scroll_container_per_panel", () => {
    expect(ratchet("scrollY").problems).toEqual([]);
  });

  it("subnav_registration_or_ledger", () => {
    const ids = MODULES.map((m) => m.id);
    expect(ids.length, "MODULES 是 SUBNAV 穷尽性的编译期判据").toBe(14);
    const empty = ids.filter((id) => SUBNAV[id].length === 0);
    const unregistered = empty.filter((id) => !PENDING_SUBNAV.some((p) => p.id === id));
    expect(unregistered, "空 SUBNAV 未登记待拆批次").toEqual([]);
    const stale = PENDING_SUBNAV.filter((p) => SUBNAV[p.id].length > 0).map((p) => p.id);
    expect(stale, "已登记但 SUBNAV 已非空——删条目（反向过期守卫）").toEqual([]);
    const dupes = PENDING_SUBNAV.map((p) => p.id).filter(
      (id, i, arr) => arr.indexOf(id) !== i,
    );
    expect(dupes, "待拆台账重复登记").toEqual([]);
  });

  it("shell_skeleton_is_the_spec_not_a_handroll", () => {
    const shell = shellRaw["/src/windows/MainWorkbench.tsx"];
    expect(shell, "glob 未读到主工作台源码").toBeTruthy();
    // 1 节骨架：末行 32（状态栏高）而非 28；两处头部一律经组件族，主操作恒最右
    expect(shell).toContain('gridTemplateRows: "40px 44px 1fr 32px"');
    expect(count(shell, "<PanelHeader\\b")).toBe(2);
    expect(shell).not.toMatch(/styles\.head\b/);
    expect(shell).toContain('data-nf="work"');
  });

  it("subnav_compact_rail_reads_the_variable_contract", () => {
    // D-42 风险④的组件半侧机理钉：griffel 运行时注入的类与 global.css 同特异度且次序在后，
    // 直接写 width/display 会被静默压掉（真机实测 206.8px 三档不变）。
    // 唯一不依赖层叠次序的通路＝global.css 设变量、组件读变量＋字面量兜底。
    // 这里钉"读侧"仍按变量契约取数；global.css 的"下发侧"由 tools/assert-patterns.ps1 的
    // D-42 扫描成对钉住（vitest 读不到 .css 原文，实测见 SUBNAV_VARS 注释）。
    const subnav = shellRaw["/src/layout/SubNav.tsx"];
    expect(subnav, "glob 未读到二级导航源码").toBeTruthy();
    for (const v of SUBNAV_VARS) expect(subnav, `SubNav 缺变量读取 ${v}`).toContain(v);
    // 窄栏判据的字面兜底档（真机走查 190/48/48 的静态对拍面）
    expect(subnav).toContain("var(--nf-subnav-w, 190px)");
    expect(subnav).toContain('data-nf="subnav"');
    expect(subnav).toContain('data-nf="subnav-group"');
    expect(subnav).toContain('data-nf="subnav-label"');
  });
});
