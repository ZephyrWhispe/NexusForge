import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SysPanel, { winopsEffectHint } from "../SysPanel";
import {
  sysCleanScan,
  sysCleanTargets,
  sysMetricsHistory,
  winopsCatalog,
  winopsScan,
  type CleanScanItemDto,
  type CleanTargetDto,
  type WinopsTweakDto,
} from "../../../ipc/client";
import { useSession } from "../../../stores/session";

// D-29 B1/T-B1-9 回归：清理页扫描前即渲染 sys_clean_targets 静态清单
// （dir/exts/optional 首次可见、safe_default 只做「推荐」角标绝不自动勾选）；
// 勾选态持久化 = session store 新键 sysCleanSelected（旧快照缺键回退 []）；
// 调整页「浏览目录」Dialog 按 category 分组并带 maintenance/需管理员/生效方式。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    sysMetricsHistory: vi.fn(),
    sysCleanTargets: vi.fn(),
    sysCleanScan: vi.fn(),
    winopsCatalog: vi.fn(),
    winopsScan: vi.fn(),
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
  {
    id: "windows_temp",
    label: "系统临时文件（C:\\Windows\\Temp，需管理员）",
    dir: "C:\\Windows\\Temp",
    exts: [],
    need_admin: true,
    safe_default: false,
    optional: true,
  },
  {
    id: "update_cache",
    label: "Windows 更新下载缓存（需管理员）",
    dir: "C:\\Windows\\SoftwareDistribution\\Download",
    exts: [],
    need_admin: true,
    safe_default: false,
    optional: true,
  },
  {
    id: "thumb_cache",
    label: "缩略图/图标缓存（占用中文件自动跳过）",
    dir: "C:\\Users\\me\\AppData\\Local\\Microsoft\\Windows\\Explorer",
    exts: ["db"],
    need_admin: false,
    safe_default: false,
    optional: true,
  },
];

const scanned = (id: string, files: number, safe: boolean): CleanScanItemDto => ({
  target_id: id,
  label: id,
  need_admin: false,
  safe_default: safe,
  files,
  reclaim_bytes: files * 1024,
  skipped_recent: 0,
  missing: false,
});

const tweak = (
  id: string,
  name: string,
  category: string,
  extra: Partial<WinopsTweakDto> = {},
): WinopsTweakDto => ({
  id,
  name,
  category,
  description: `${name} 的说明`,
  requires_admin: false,
  maintenance: false,
  actions: [{ type: "registry" }],
  ...extra,
});

let container: HTMLDivElement;
let root: Root;

function buttonByText(
  text: string,
  scope: ParentNode = container,
): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

function execButton(): HTMLButtonElement {
  const btn = [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.trim().startsWith("执行清理"),
  );
  expect(btn).toBeDefined();
  return btn as HTMLButtonElement;
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SysPanel />);
  });
  await act(async () => {});
}

/** D-43 C6：互斥视图上移左轨后，面板本体不再自带 Tabs。测试走左轨派发的那条腿——
 *  setSysTab（view 选择态在 session 分键里，面板读同一键），判据强度不变。 */
async function switchTab(id: string) {
  await act(async () => {
    useSession.getState().setSysTab(id);
  });
  await act(async () => {});
}

/** 清理页复选框：index 0 为工具栏「移入回收站」，其后按清单顺序 */
function targetCheckbox(idx: number): HTMLInputElement {
  const boxes = container.querySelectorAll('input[type="checkbox"]');
  return boxes[idx + 1] as HTMLInputElement;
}

/** 复选框所在清单行（Fluent Checkbox 内部全是 span，closest div 即 styles.item 行） */
function rowOf(box: HTMLInputElement): HTMLElement {
  const row = box.closest("div");
  expect(row, "checkbox 应位于清单行内").not.toBeNull();
  return row as HTMLElement;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(sysMetricsHistory).mockResolvedValue([]);
  vi.mocked(sysCleanTargets).mockResolvedValue(TARGETS);
  vi.mocked(sysCleanScan).mockResolvedValue([]);
  vi.mocked(winopsScan).mockResolvedValue([]);
  vi.mocked(winopsCatalog).mockResolvedValue([]);
  useSession.getState().setSysCleanSelected([]);
  // 视图选择态是 session 持久键，用例之间会互相留下 ⇒ 每例显式复位到默认监控档
  useSession.getState().setSysTab("monitor");
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

describe("SysPanel 清理静态清单 + 目录浏览（T-B1-9）", () => {
  it("sysCleanTargets_listBeforeScan：未扫描即渲染 4 目标含 dir，扫描后仍不自动勾选", async () => {
    await mount();
    await switchTab("clean");
    expect(sysCleanTargets).toHaveBeenCalledTimes(1);
    // 三字段补全首次可见：dir / exts / optional（行内判据，互不串扰）
    expect(rowOf(targetCheckbox(0)).textContent).toContain(
      "C:\\Users\\me\\AppData\\Local\\Temp",
    );
    expect(rowOf(targetCheckbox(0)).textContent).toContain("全部文件");
    expect(rowOf(targetCheckbox(3)).textContent).toContain("仅 .db");
    expect(rowOf(targetCheckbox(1)).textContent).toContain("缺失自动跳过");
    // safe_default 只做「推荐」角标：正例在 user_temp 行，其余三行不得出现
    expect(rowOf(targetCheckbox(0)).textContent).toContain("推荐");
    for (const i of [1, 2, 3]) expect(rowOf(targetCheckbox(i)).textContent).not.toContain("推荐");
    // 未扫描不得执行：按钮禁用 + tooltip 如实
    expect(execButton().disabled).toBe(true);
    expect(execButton().title).toContain("未扫描仅见清单，扫描后方可执行");
    // 勾选 = 持久偏好，即时写进 session store
    await click(targetCheckbox(0));
    await click(targetCheckbox(3));
    expect(useSession.getState().sysCleanSelected).toEqual(["user_temp", "thumb_cache"]);
    // 扫描后：safe_default 依旧不自动勾选，只剔除本次不可执行的持久勾选
    vi.mocked(sysCleanScan).mockResolvedValue([
      scanned("user_temp", 12, true),
      scanned("thumb_cache", 0, false),
    ]);
    await click(buttonByText("扫描")!);
    expect(container.textContent).toContain("可清理 12 文件");
    expect(useSession.getState().sysCleanSelected).toEqual(["user_temp"]);
    expect(execButton().disabled).toBe(false);
  });

  it("winopsCatalog_browseGroupedByCategory_withHints：分组键=category，maintenance 徽标+生效方式映射", async () => {
    vi.mocked(winopsCatalog).mockResolvedValue([
      tweak("t1", "隐藏任务栏搜索图标", "任务栏"),
      tweak("t2", "禁用任务栏 Widgets", "任务栏", { requires_admin: true }),
      tweak("t3", "清理 SoftwareDistribution 下载缓存", "维护", {
        maintenance: true,
        actions: [{ type: "file_clean" }],
      }),
    ]);
    await mount();
    await switchTab("tweaks");
    // 懒加载：未点「浏览目录」不得 invoke，且绝不借道 winops_scan
    expect(winopsCatalog).not.toHaveBeenCalled();
    await click(buttonByText("浏览目录")!);
    expect(winopsCatalog).toHaveBeenCalledTimes(1);
    expect(winopsScan).not.toHaveBeenCalled();
    const dialog = document.body;
    // 分组键 = category（非 family），计数随组
    expect(dialog.textContent).toContain("任务栏（2）");
    expect(dialog.textContent).toContain("维护（1）");
    expect(dialog.textContent).toContain("维护型"); // DTO 补 maintenance 字段的回归位
    expect(dialog.textContent).toContain("需管理员");
    expect(dialog.textContent).toContain("生效方式：注册表写入");
    expect(dialog.textContent).toContain("文件清理（维护型动作");
    // 纯函数直钉：多动作拼接与未知兜底
    expect(winopsEffectHint([{ type: "service" }, { type: "restore_point" }])).toContain(
      "服务启停/启动类型变更",
    );
    expect(winopsEffectHint([])).toBe("生效方式：见条目说明");
  });

  it("sysCleanSelected_persistedRoundtrip：partialize 落盘含键、旧快照缺键回退 []", async () => {
    useSession.getState().setSysCleanSelected(["user_temp", "thumb_cache"]);
    const raw = JSON.parse(localStorage.getItem("nf-session") ?? "{}") as {
      state?: Record<string, unknown>;
    };
    expect(raw.state?.sysCleanSelected).toEqual(["user_temp", "thumb_cache"]);
    // 其余持久键不受影响（partialize 只增不改）
    expect(raw.state).toHaveProperty("themeMode");
    // 旧快照（无 sysCleanSelected 键）+ 内存复位 = 模拟升级后首启：回退 []。
    // 注意 jsdom 的 setItem 会同步派发 storage 事件 → session.ts 监听器对每次
    // 写入都 void 启动一条水合链，最后完成者胜；故先复位内存勾选、再播种快照，
    // 并以显式 StorageEvent 走生产同款跨窗口水合通道，末端链只读旧快照。
    useSession.getState().setSysCleanSelected([]);
    const legacy = JSON.stringify({
      state: { themeMode: "light", activeModule: "clipboard", clipGroup: "all" },
      version: 0,
    });
    localStorage.setItem("nf-session", legacy);
    window.dispatchEvent(new StorageEvent("storage", { key: "nf-session", newValue: legacy }));
    // zustand v5 水合链不随调用者 await 透出（生产代码同样 void），排空微任务
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    // 正对照：rehydrate 确实发生了（themeMode 已变 light），排除「测试假绿」
    expect(useSession.getState().themeMode).toBe("light");
    expect(useSession.getState().sysCleanSelected).toEqual([]);
  });
});
