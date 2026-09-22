import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SettingsSection from "../panels/SettingsSection";
import {
  clipboardCaptureGet,
  clipboardExport,
  clipboardGroupCounts,
  clipboardImport,
  clipboardStats,
  hostConfigGet,
  hostConfigSet,
  type ClipExportResult,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";

// D-29 B3/T-B3-9 回归（09 §8.2）：备份两卡的前端语义。
// 红线口径是"同一道门的两侧"：勾选含敏感条目而口令不足 8 字符时**前端就不发命令**，
// 宿主的 CLIPBOARD_EXPORT_001 是第二道；导入未确认（取消）时零 invoke。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    clipboardCaptureGet: vi.fn(),
    clipboardStats: vi.fn(),
    clipboardGroupCounts: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    clipboardExport: vi.fn(),
    clipboardImport: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));
vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));
vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn() };
});
// 设置表单引擎另有其自己的挂载测，此处只关心备份两卡
vi.mock("../../../settings/SchemaForm", () => ({ default: () => null }));

let container: HTMLDivElement;
let root: Root;

const EXPORT_OK: ClipExportResult = {
  path: "C:\\appdata\\NexusForge\\export\\clipboard-1700000000000.nfclip.json",
  entries: 12,
  secrets: 3,
  images_skipped: 1,
};

async function mount() {
  await act(async () => {
    root.render(<SettingsSection />);
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

/** 原生 setter 赋值：React 受控 input 只认这条路径 */
async function typeInto(label: string, value: string) {
  const box = container.querySelector<HTMLInputElement>(`[aria-label="${label}"]`);
  if (!box) throw new Error(`输入框 ${label} 未渲染`);
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter!.call(box, value);
    box.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => {});
}

async function checkSecrets(on = true) {
  // Fluent 的 Checkbox 把 label 作为 input 的兄弟节点（靠 for/id 关联），不是祖先
  const lbl = [...container.querySelectorAll("label")].find((l) =>
    (l.textContent ?? "").includes("包含敏感条目"),
  );
  const box = lbl && container.querySelector<HTMLInputElement>(`#${lbl.getAttribute("for")}`);
  if (!box) throw new Error("「包含敏感条目」勾选框未渲染");
  if (box.checked === on) return;
  await act(async () => {
    box.click();
  });
  await act(async () => {});
}

const buttonByText = (text: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);

const textOf = () => container.textContent ?? "";

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardStats).mockResolvedValue({
    total: 0,
    by_content_type: {},
    by_group: {},
    top_source_apps: [],
    bytes_blob: 0,
  });
  vi.mocked(hostConfigGet).mockResolvedValue({ block_patterns: [] });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
  vi.mocked(clipboardGroupCounts).mockResolvedValue({});
  vi.mocked(clipboardExport).mockResolvedValue(EXPORT_OK);
  vi.mocked(clipboardImport).mockResolvedValue({
    imported: 4,
    duplicates: 1,
    secrets: 2,
    images_skipped: 0,
  });
  vi.mocked(confirmAction).mockResolvedValue(true);
  vi.mocked(notify).mockClear();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.clearAllMocks();
});

describe("T-B3-9 备份导出/导入卡", () => {
  it("exportDialog_requiresPassphraseWhenSecretsChecked", async () => {
    await mount();
    await checkSecrets(true);
    await typeInto("备份口令", "短");

    // 内联拦下的可见理由（不是只把按钮变灰了事）
    expect(textOf()).toContain("口令须至少 8 字符");
    const btn = buttonByText("导出备份");
    expect(btn).toBeTruthy();
    expect(btn!.disabled).toBe(true);
    await click(btn!);
    expect(clipboardExport).not.toHaveBeenCalled();

    // 正对照：补够口令即放行，且参数按勾选如实传下去
    await typeInto("备份口令", "十六字符以上的口令");
    expect(textOf()).not.toContain("口令须至少 8 字符");
    await click(buttonByText("导出备份")!);
    expect(clipboardExport).toHaveBeenCalledWith("十六字符以上的口令", true);
  });

  it("exportDialog_success_showsPathAndCopyButton", async () => {
    await mount();
    await typeInto("备份口令", "十六字符以上的口令");
    await click(buttonByText("导出备份")!);

    expect(textOf()).toContain(EXPORT_OK.path);
    // 计数走通知（卡片只留路径与复制钮）：三枚计数都得如实出现，图片外置那条不许静默
    expect(notify).toHaveBeenCalledWith(
      "success",
      "备份已导出",
      "12 条（含敏感 3 条，图片/blob 外置 1 条未随文件）",
    );

    const copied: string[] = [];
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: (s: string) => Promise.resolve(copied.push(s)) },
    });
    await click(buttonByText("复制路径")!);
    expect(copied).toEqual([EXPORT_OK.path]);
    expect(notify).toHaveBeenCalledWith("success", "已复制备份路径");
  });

  it("importDialog_pathAndPassphrase_invokesAndReloads", async () => {
    await mount();
    await typeInto("备份文件路径", "C:\\tmp\\clipboard-1.nfclip.json");
    await typeInto("导入口令", "十六字符以上的口令");
    await click(buttonByText("导入备份")!);

    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(clipboardImport).toHaveBeenCalledWith("C:\\tmp\\clipboard-1.nfclip.json", "十六字符以上的口令");
    expect(notify).toHaveBeenCalledWith(
      "success",
      "备份已导入",
      "新增 4 条 · 重复 1 条 · 敏感 2 条",
    );
    // 导入改的是整库：统计卡须重拉（同一 refresh 即"历史与计数都刷新"的本地半边）
    expect(clipboardStats).toHaveBeenCalledTimes(2);
  });

  it("importDialog_cancelZeroInvoke", async () => {
    vi.mocked(confirmAction).mockResolvedValue(false);
    await mount();
    await typeInto("备份文件路径", "C:\\tmp\\clipboard-1.nfclip.json");
    await typeInto("导入口令", "十六字符以上的口令");
    await click(buttonByText("导入备份")!);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(clipboardImport).not.toHaveBeenCalled();
    expect(notify).not.toHaveBeenCalled();

    // 正对照：路径空时连确认都不该弹（钮 disabled），有路径才走到确认那一步
    await typeInto("备份文件路径", "");
    expect(buttonByText("导入备份")!.disabled).toBe(true);
  });
});
