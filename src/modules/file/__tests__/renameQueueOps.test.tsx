import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import FilePanel, { renameConflictReasons, zipTarget } from "../FilePanel";
import {
  fileBreadcrumbs,
  fileDrives,
  fileEnqueue,
  fileOpsActive,
  fileOpsPending,
  fileOpDropPending,
  filePreview,
  fileRenameApply,
  fileRenameEntry,
  fileRenamePlan,
  fileSearch,
  fileList,
  type FileEntryDto,
  type PendingOpDto,
  type RenamePlanDto,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";
import { useSession } from "../../../stores/session";

// D-29 B1/T-B1-5 回归：批量重命名（预览表+前端推导冲突原因+勾选应用）、等待队列
// 丢弃、压缩/解压入队 kind 字面透传（TS 联合放开到 FileOpKind 的接线证明）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    fileList: vi.fn(),
    fileBreadcrumbs: vi.fn(),
    fileDrives: vi.fn(),
    fileSearch: vi.fn(),
    filePreview: vi.fn(),
    fileEnqueue: vi.fn(),
    fileOpsActive: vi.fn(),
    fileOpsPending: vi.fn(),
    fileOpDropPending: vi.fn(),
    fileRenamePlan: vi.fn(),
    fileRenameApply: vi.fn(),
    fileRenameEntry: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

function entry(name: string, isDir: boolean): FileEntryDto {
  return {
    name,
    path: `C:\\dir\\${name}`,
    is_dir: isDir,
    size: isDir ? 0 : 1024,
    modified_ms: Date.parse("2026-09-19T10:00:00"),
    ext: isDir ? "" : name.split(".").pop() ?? "",
    hidden: false,
  };
}

function plan(from: string, to: string, conflict: boolean): RenamePlanDto {
  return { from: `C:\\dir\\${from}`, to: `C:\\dir\\${to}`, conflict };
}

function pendingRow(opId: string, over: Partial<PendingOpDto> = {}): PendingOpDto {
  return {
    op_id: opId,
    kind: "copy",
    srcs: ["C:\\big.bin"],
    dst: "D:\\bak\\big.bin",
    policy: "ask",
    recycle: false,
    file_index: 3,
    bytes_done: 1024,
    created_ms: Date.parse("2026-09-18T22:40:00"),
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}
function buttonByPrefix(prefix: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) =>
    (b.textContent?.trim() ?? "").startsWith(prefix),
  );
}
function rowByText(text: string): Element | undefined {
  return [...container.querySelectorAll("tr")].find((r) => r.textContent?.includes(text));
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileList).mockResolvedValue([
    entry("docs", true),
    entry("a.txt", false),
    entry("pack.zip", false),
  ]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([
    ["C:", "C:\\"],
    ["dir", "C:\\dir"],
  ]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileSearch).mockResolvedValue({ hits: [], degraded: false });
  vi.mocked(filePreview).mockResolvedValue({ kind: "unsupported", reason: "未配置" });
  vi.mocked(fileOpsActive).mockResolvedValue([]);
  vi.mocked(fileOpsPending).mockResolvedValue([]);
  vi.mocked(fileOpDropPending).mockResolvedValue(true);
  vi.mocked(fileEnqueue).mockResolvedValue({ op_id: "op-new", conflicts: [] });
  vi.mocked(fileRenamePlan).mockResolvedValue([]);
  vi.mocked(fileRenameApply).mockResolvedValue(0);
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
  useSession.setState({ fileSubPanel: "browse" });
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<FilePanel />);
  });
  await act(async () => {});
}

describe("FilePanel 批量重命名+等待队列+压缩解压（T-B1-5）", () => {
  it("renamePlan_previewTableAndApplySendsCheckedPlans：预览表+前端推导原因+应用只发勾选条目", async () => {
    vi.mocked(fileRenamePlan).mockResolvedValue([
      plan("a.txt", "a1.txt", false),
      plan("b.txt", "x.txt", true),
      plan("c.txt", "x.txt", true),
      plan("d.txt", "taken.txt", true),
      plan("e.txt", "e.txt", false),
    ]);
    vi.mocked(fileRenameApply).mockResolvedValue(1);
    await mount();
    await click(buttonByText("批量重命名")!);
    await click(buttonByText("生成预览")!);
    // dir 未选中任何文件 → names=[]（服务端按目录列全部文件）
    expect(fileRenamePlan).toHaveBeenCalledWith("C:\\", [], {
      template: "{name}{ext}",
      regex: null,
      replacement: "",
      case: "none",
      start: 1,
    });
    // 冲突原因前端推导：计划内同名目标=表内重复，其余冲突=目标已存在；no-op=不变
    expect(document.body.textContent).toContain("表内重复");
    expect(document.body.textContent).toContain("目标已存在");
    expect(document.body.textContent).toContain("不变");
    expect(document.body.textContent).toContain("应用（勾选 1 项）");
    // （实测本 Fluent 版本 Checkbox 不把 aria-label 透传到原生 input，取序断言：
    // 勾选框数量恰等于计划数即证明每行恰一控件）
    const cbs = [...document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
    expect(cbs).toHaveLength(5);
    expect(cbs[0]?.checked).toBe(true);
    expect(cbs[1]?.checked).toBe(false);
    expect(cbs[2]?.checked).toBe(false);
    expect(cbs[3]?.checked).toBe(false);
    expect(cbs[4]?.checked).toBe(false);
    await click(buttonByPrefix("应用")!);
    expect(fileRenameApply).toHaveBeenCalledTimes(1);
    expect(fileRenameApply).toHaveBeenCalledWith([plan("a.txt", "a1.txt", false)]);
    expect(notify).toHaveBeenCalledWith(
      "success",
      "批量重命名完成",
      expect.stringContaining("已重命名 1 项"),
    );
  });

  it("renamePlan_badTemplate_surfacesFILE_RENAME_001：非法模板错误内联可见且应用保持禁用（负例）", async () => {
    vi.mocked(fileRenamePlan).mockRejectedValue({
      kind: "Module",
      data: { code: "FILE_RENAME_001", message: "重命名规则错误: 未知变量: {unknown}" },
    });
    await mount();
    await click(buttonByText("批量重命名")!);
    const tpl = document.querySelector<HTMLInputElement>('input[aria-label="重命名模板"]');
    expect(tpl).not.toBeNull();
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setter.call(tpl, "{unknown}");
      tpl!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(buttonByText("生成预览")!);
    expect(fileRenamePlan).toHaveBeenCalledWith("C:\\", [], expect.objectContaining({ template: "{unknown}" }));
    const body = document.body.textContent ?? "";
    expect(body).toContain("FILE_RENAME_001");
    expect(body).toContain("未知变量");
    // 错误后不得留有可应用的假计划表
    expect(fileRenameApply).not.toHaveBeenCalled();
    expect(buttonByPrefix("应用")?.disabled).toBe(true);
  });

  it("compressExtract_enqueueKindLiteralPassThrough：fileEnqueue 实收 kind 字面 compress/extract（负例式）", async () => {
    await mount();
    // 选中 a.txt → 压缩：目标输入留空 → 当前目录自动 <主名>.zip
    await click(rowByText("a.txt")!);
    await click(buttonByText("压缩为 zip")!);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    const specC = vi.mocked(fileEnqueue).mock.calls[0][0];
    expect(specC.kind).toBe("compress");
    expect(specC.srcs).toEqual(["C:\\dir\\a.txt"]);
    expect(specC.dst).toBe("C:\\a.zip");
    // 换选 pack.zip → 解压：显式 rename 策略（后端解压冲突安全默认），源恰为该 zip
    await click(rowByText("a.txt")!);
    await click(rowByText("pack.zip")!);
    await click(buttonByText("解压")!);
    const specX = vi.mocked(fileEnqueue).mock.calls[1][0];
    expect(specX.kind).toBe("extract");
    expect(specX.srcs).toEqual(["C:\\dir\\pack.zip"]);
    expect(specX.policy).toBe("rename");
    // 行内重命名走 file_rename_entry（单项、非队列）：此刻选中集仍是 pack.zip
    const dst = container.querySelector<HTMLInputElement>(
      'input[placeholder="目标目录（复制/移动用）"]',
    );
    expect(dst).not.toBeNull();
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    await act(async () => {
      setter.call(dst, "renamed.zip");
      dst!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(buttonByText("重命名")!);
    expect(fileRenameEntry).toHaveBeenCalledWith("C:\\dir\\pack.zip", "C:\\renamed.zip");
  });

  it("fileOpsPending_dropInvokesAndReloads：挂载即拉等待队列，丢弃后恰一次重拉", async () => {
    // T-B6-10 三档分派：等待队列整块原样搬入传输档（判据一字未动），观察需停在 transfers
    useSession.setState({ fileSubPanel: "transfers" });
    vi.mocked(fileOpsPending)
      .mockResolvedValueOnce([pendingRow("op-9")])
      .mockResolvedValue([]);
    await mount();
    expect(fileOpsPending).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain("等待中");
    expect(container.textContent).toContain("big.bin");
    expect(container.textContent).toContain("D:\\bak\\big.bin");
    await click(buttonByText("丢弃")!);
    expect(fileOpDropPending).toHaveBeenCalledWith("op-9");
    expect(fileOpsPending).toHaveBeenCalledTimes(2);
    expect(notify).toHaveBeenCalledWith("success", "已丢弃等待记录", "op-9");
    // 第二轮（已空）不再渲染丢弃按钮
    expect(buttonByText("丢弃")).toBeUndefined();
  });

  it("renameHelpers_pureFunctions_pinReasonsAndZipTargets：纯函数判据（大小写不敏感/裸盘符/完整路径）", () => {
    const reasons = renameConflictReasons([
      plan("b.txt", "X.txt", true),
      plan("c.txt", "x.TXT", true), // 大小写混合目标 → Windows 语义同名，表内重复
      plan("d.txt", "taken.txt", true),
      plan("e.txt", "e.txt", false),
    ]);
    expect(reasons.get("C:\\dir\\b.txt")).toBe("表内重复");
    expect(reasons.get("C:\\dir\\c.txt")).toBe("表内重复");
    expect(reasons.get("C:\\dir\\d.txt")).toBe("目标已存在");
    expect(reasons.has("C:\\dir\\e.txt")).toBe(false);
    expect(zipTarget("C:\\dir", "", "photo")).toBe("C:\\dir\\photo.zip");
    expect(zipTarget("C:\\dir", "D:\\out\\", "photo")).toBe("D:\\out\\photo.zip");
    expect(zipTarget("C:\\dir", "D:", "photo")).toBe("D:\\photo.zip");
    expect(zipTarget("C:\\dir", "D:\\packs\\final.zip", "photo")).toBe("D:\\packs\\final.zip");
  });
});
