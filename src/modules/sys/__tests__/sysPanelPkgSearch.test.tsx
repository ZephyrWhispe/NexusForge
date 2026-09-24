import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SysPanel from "../SysPanel";
import {
  sysCleanTargets,
  sysMetricsHistory,
  sysPkgAction,
  sysPkgCmdPreview,
  sysPkgList,
  sysPkgSearch,
  sysPkgSources,
  sysProcesses,
  type PkgEntryDto,
  type PkgSearchRowDto,
  type PkgSourceDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// D-29 B7/T-B7-12 回归：包管理在线搜索——搜索结果表渲染 + 安装钮复用确切命令行
// 确认对话框（cmd_preview→confirmAction→sys_pkg_action 链）；已装表每行「升级」钮
// 走单包 upgrade 动作。修前：无在线搜索面（只有本地过滤）。

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
    sysPkgCmdPreview: vi.fn(),
    sysPkgAction: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(),
}));

const SOURCES: PkgSourceDto[] = [
  { id: "winget", label: "winget（系统内置）", available: true },
  { id: "scoop", label: "Scoop（用户级）", available: false },
];
const INSTALLED: PkgEntryDto[] = [
  { id: "Git.Git", name: "Git", version: "2.47.1", available: "2.48.0", source: "winget" },
];
const RESULTS: PkgSearchRowDto[] = [
  { id: "Microsoft.PowerToys", name: "PowerToys", version: "0.87.0", source: "winget" },
];

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function setInputValue(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/** 监控 Tab 两拍装载等后台定时器排空（600ms 间隔 + 余量） */
async function settle() {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 800));
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(sysMetricsHistory).mockResolvedValue([]);
  vi.mocked(sysCleanTargets).mockResolvedValue([]);
  vi.mocked(sysProcesses).mockResolvedValue([]);
  vi.mocked(sysPkgSources).mockResolvedValue(SOURCES);
  vi.mocked(sysPkgList).mockResolvedValue(INSTALLED);
  vi.mocked(sysPkgSearch).mockResolvedValue(RESULTS);
  vi.mocked(sysPkgCmdPreview).mockResolvedValue(
    "winget install --id Microsoft.PowerToys --exact --silent",
  );
  vi.mocked(sysPkgAction).mockResolvedValue(["Ok."]);
  vi.mocked(confirmAction).mockResolvedValue(true);
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

async function mountPkgTab() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SysPanel />);
  });
  await settle();
  await click(buttonByText("包管理")!);
  await act(async () => {});
  // 包 Tab 清单为手动装载（既有形制）：先「刷新清单」
  await click(buttonByText("刷新清单")!);
  await act(async () => {});
}

function searchInput(): HTMLInputElement {
  // Fluent Input 的 className 挂在包装 span 上，真 input 走 placeholder 定位
  const el = [...container.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith("包名关键词"),
  );
  expect(el, "包管理 Tab 应有在线搜索输入框").not.toBeUndefined();
  return el!;
}

describe("SysPanel 在线搜索与单包升级（T-B7-12）", () => {
  it("sysPanel_pkgSearchDialog_listsResults_installBtn：搜索出表→安装钮走预览确认链；已装行升级钮走 upgrade", async () => {
    await mountPkgTab();
    // 已装表在册（升级钮每行必有）
    expect(container.textContent).toContain("Git");
    const results0 = container.querySelector('[data-testid="pkg-search-results"]');
    expect(results0, "未搜索前不出结果表").toBeNull();

    setInputValue(searchInput(), " power toys ");
    await act(async () => {});
    await click(buttonByText("搜索")!);

    // 首可用源（winget 优先）+ trim 后原样传递
    expect(sysPkgSearch).toHaveBeenCalledTimes(1);
    expect(vi.mocked(sysPkgSearch).mock.calls[0]).toEqual(["winget", "power toys"]);
    const results = container.querySelector('[data-testid="pkg-search-results"]');
    expect(results, "搜索后结果表在册").not.toBeNull();
    expect(results!.textContent).toContain("PowerToys");
    expect(results!.textContent).toContain("Microsoft.PowerToys · 0.87.0 · winget");

    // 结果表内安装钮（scoped 查询防误撞已装表安装钮）
    const installBtn = [...results!.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "安装",
    )!;
    await click(installBtn);
    // 在线安装对话框链：预览先行（所见即所跑），确认后才执行
    expect(sysPkgCmdPreview).toHaveBeenCalledWith("winget", "install", "Microsoft.PowerToys");
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(sysPkgAction).toHaveBeenCalledTimes(1);
    expect(vi.mocked(sysPkgAction).mock.calls[0]).toEqual([
      "winget",
      "install",
      "Microsoft.PowerToys",
    ]);
    await settle();

    // 已装表「升级」钮：单包 upgrade 动作走同一预览确认链
    const upgradeBtn = buttonByText("升级")!;
    await click(upgradeBtn);
    expect(sysPkgCmdPreview).toHaveBeenCalledWith("winget", "upgrade", "Git.Git");
    expect(vi.mocked(sysPkgAction).mock.calls[1]).toEqual(["winget", "upgrade", "Git.Git"]);
    await settle();
  });

  it("sysPanel_pkgSearchDialog_declinedPreview_zeroAction：确认对话框否决则 sys_pkg_action 零触达", async () => {
    await mountPkgTab();
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    setInputValue(searchInput(), "powertoys");
    await click(buttonByText("搜索")!);
    const results = container.querySelector('[data-testid="pkg-search-results"]')!;
    const installBtn = [...results.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "安装",
    )!;
    await click(installBtn);
    expect(sysPkgCmdPreview).toHaveBeenCalledTimes(1);
    expect(sysPkgAction, "用户否决安装对话框后不得触达变更命令").not.toHaveBeenCalled();
    await settle();
  });
});
