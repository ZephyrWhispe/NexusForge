import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SysPanel from "../SysPanel";
import sysPanelSrc from "../SysPanel.tsx?raw";
import { SUBNAV } from "../../../layout/modules";
import { isSysTab, useSession } from "../../../stores/session";
import {
  sysCleanTargets,
  sysMetricsHistory,
  sysPkgList,
  sysPkgSearch,
  sysPkgSources,
  sysProcesses,
  type CleanTargetDto,
  type PkgEntryDto,
  type PkgSearchRowDto,
} from "../../../ipc/client";

// D-43 C6：系统管理面板重排的判据面。四件事各钉一枚——
// ① 左轨注册表与面板视图门控对合（SUBNAV.sys 四枚 id 必须全是 SysTab，否则左轨出现
//    点了没反应的撒谎按钮）；视图互斥⇒view 形制，不是 anchor；
// ② 每视图恰好一枚吸顶工具条，主按钮恒末位（规范 5 节）；
// ③ 内层 maxHeight 视口撤除后，长清单靠分页显影，页脚报"已装 n · 在场 k"而非静默截断；
//    包管理输出的末 30 行截断 jsdom 无从造（事件腿只在真窗注册），按源码形状钉；
// ④ 选择态真进持久化快照（C5 曾声称 termTab 入 partialize 而文件里从未落，本批补正）。
// 像素/滚动一律不在此证明（jsdom 无布局引擎），归 C9 真机 CDP 走查。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    sysMetricsHistory: vi.fn(),
    sysCleanTargets: vi.fn(),
    sysProcesses: vi.fn(),
    sysPkgSources: vi.fn(),
    sysPkgList: vi.fn(),
    sysPkgSearch: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const TARGETS: CleanTargetDto[] = [
  {
    id: "user_temp",
    label: "用户临时文件（%TEMP%）",
    dir: "C:\\Users\\me\\AppData\\Local\\Temp",
    exts: [],
    need_admin: false,
    safe_default: true,
    optional: false,
  },
];

/** 视图 id → 该视图工具条的主按钮文本（末位判据） */
const PRIMARY: Record<string, string> = {
  monitor: "刷新",
  clean: "执行清理（0 项）",
  pkg: "全部升级",
  tweaks: "扫描",
};

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string, scope: ParentNode = document): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function switchTab(id: string) {
  await act(async () => {
    useSession.getState().setSysTab(id);
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SysPanel />);
  });
  // 进程页两拍采样自带 600ms 定时器（挂载即起，与当前视图无关）：在 act 内排空这一拍，
  // 否则 setProcs 落在测试体之外＝act 警告噪音（形同 sysPanelPkgSearch 的 settle，与判据无关）
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 700));
  });
  await act(async () => {});
}

/** 区块根（Section 自带 data-nf="sec"），标题＝head 行首个 Text（span），不含操作区文案 */
function sectionTitles(): string[] {
  return [...container.querySelectorAll('[data-nf="sec"]')].map(
    (el) => el.querySelector("span")?.textContent?.trim() ?? "",
  );
}

function setInputValue(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function onlineSearchInput(): HTMLInputElement {
  const el = [...container.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith("包名关键词"),
  );
  expect(el, "包管理视图应有在线搜索输入框").not.toBeUndefined();
  return el!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(sysMetricsHistory).mockResolvedValue([]);
  vi.mocked(sysProcesses).mockResolvedValue([]);
  vi.mocked(sysCleanTargets).mockResolvedValue(TARGETS);
  vi.mocked(sysPkgSources).mockResolvedValue([
    { id: "winget", label: "winget（系统内置）", available: true },
  ]);
  vi.mocked(sysPkgList).mockResolvedValue([]);
  vi.mocked(sysPkgSearch).mockResolvedValue([]);
  act(() => useSession.getState().setSysTab("monitor"));
  // 勾选态也是持久键：主按钮计数判据按"零勾选"写，必须先归零
  useSession.getState().setSysCleanSelected([]);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  vi.clearAllMocks();
});

describe("SysPanel 重排（D-43 C6）", () => {
  it("sysLayout_registryCoversGatedViews_andMutualExclusionHolds", async () => {
    const items = SUBNAV.sys.flatMap((s) => s.items);
    expect(items.map((i) => i.scope)).toEqual(["view", "view", "view", "view"]);
    for (const i of items) expect(isSysTab(i.id), `${i.id} 不是 SysTab`).toBe(true);

    await mount();
    // 缺省档 monitor：两区块各有标题（首层无题＝"直接放在页面上"的形态学根因）
    expect(sectionTitles()).toEqual(["资源监控", "进程（0）"]);
    expect(document.body.textContent).not.toContain("清理目标");

    // 四枚互斥视图逐档切：本档区块在场、他档撤场
    await switchTab("clean");
    expect(sectionTitles()).toEqual([`清理目标（${TARGETS.length}）`]);
    expect(container.querySelector('[placeholder="搜索进程名"]'), "监控档不得留在 DOM").toBeNull();

    await switchTab("pkg");
    expect(sectionTitles()).toEqual(["已装软件（0）", "在线搜索"]);

    await switchTab("tweaks");
    expect(sectionTitles()).toEqual(["系统调整（0）"]);
    expect(document.body.textContent).toContain("尚未扫描，调整项的当前状态未知");

    // 野值不落（持久化快照被改坏时确定性回落，不白屏）
    await switchTab("nope");
    expect(useSession.getState().sysTab).toBe("tweaks");
  });

  it("sysLayout_oneToolbarPerView_withPrimaryLast", async () => {
    await mount();
    for (const tab of ["monitor", "clean", "pkg", "tweaks"]) {
      await switchTab(tab);
      const toolbars = container.querySelectorAll('[role="toolbar"]');
      expect(toolbars, `${tab} 应恰有一枚工具条`).toHaveLength(1);
      const buttons = [...toolbars[0]!.querySelectorAll("button")];
      expect(buttons[buttons.length - 1]?.textContent?.trim(), `${tab} 主按钮恒末位`).toBe(
        PRIMARY[tab],
      );
    }
    // 清理档的可执行性开关在左区、主按钮仍在末位（回收站是可恢复性的唯一开关）
    await switchTab("clean");
    const bar = container.querySelector('[role="toolbar"]')!;
    expect(bar.querySelector('input[type="checkbox"]'), "回收站开关应在工具条内").not.toBeNull();
    // 三处静默截断之一：未扫描时执行钮禁用且 tooltip 如实
    const exec = [...bar.querySelectorAll("button")].find((b) =>
      b.textContent?.trim().startsWith("执行清理"),
    )!;
    expect(exec.disabled).toBe(true);
    expect(exec.title).toContain("未扫描仅见清单，扫描后方可执行");
  });

  it("sysLayout_pkgPagination_footerDisclosesTruncation", async () => {
    const rows: PkgEntryDto[] = Array.from({ length: 201 }, (_, i) => ({
      id: `pkg-${i}`,
      name: `Pkg ${i}`,
      version: "1.0.0",
      available: null,
      source: "winget",
    }));
    vi.mocked(sysPkgList).mockResolvedValue(rows);
    await switchTab("pkg");
    await mount();
    await click(buttonByText("刷新清单")!);

    // 首屏档＝200 行（内层 340px 视口撤除后的替代形制）
    expect(document.body.textContent).toContain("已装 201 项 · 在场 200 项");
    expect(document.body.textContent).toContain("pkg-199 · 1.0.0");
    expect(document.body.textContent, "超出首屏的行不得出现在场").not.toContain("pkg-200 · 1.0.0");
    const more = buttonByText("显示更多");
    expect(more, "静默截断必须给出口").toBeDefined();
    await click(more!);
    expect(document.body.textContent).toContain("已装 201 项 · 在场 201 项");
    expect(document.body.textContent).toContain("pkg-200 · 1.0.0");
    expect(buttonByText("显示更多")).toBeUndefined();
  });

  it("sysLayout_searchPagination_andOutputTailIsDisclosedInTitle", async () => {
    const hits: PkgSearchRowDto[] = Array.from({ length: 101 }, (_, i) => ({
      id: `a.${i}`,
      name: `Hit ${i}`,
      version: "1.0.0",
      source: "winget",
    }));
    vi.mocked(sysPkgSearch).mockResolvedValue(hits);
    await switchTab("pkg");
    await mount();
    // 搜索源取自「刷新清单」装载的 sources（未装载时 doOnlineSearch 拒发并如实报错，既有形制）
    await click(buttonByText("刷新清单")!);
    setInputValue(onlineSearchInput(), "notepad");
    await click(buttonByText("搜索")!);
    expect(document.body.textContent).toContain("命中 101 项 · 在场 100 项");
    expect(document.body.textContent).not.toContain("a.100 · 1.0.0");

    // 第三枚截断（输出末 30 行）jsdom 造不出数据：sys.pkg_line 只经真窗事件腿进缓冲，
    // 故按源码形状钉"行数随标题显影"，而不是假装渲得出。
    expect(sysPanelSrc).toContain(
      "包管理输出（末 ${Math.min(output.length, LOG_TAIL)} / 缓冲 ${output.length} 行）",
    );
    expect(sysPanelSrc).toMatch(/output\.slice\(-LOG_TAIL\)/);
  });

  it("sysLayout_cleanFooterCountsTargetsAndSelection", async () => {
    await switchTab("clean");
    await mount();
    expect(document.body.textContent).toContain("目标 1 项 · 已选 0 项 · 未扫描");
    expect(document.body.textContent).toContain("24h 内修改的文件自动跳过");
    await click(container.querySelectorAll('input[type="checkbox"]')[1]!);
    expect(document.body.textContent).toContain("目标 1 项 · 已选 1 项");
  });
});
