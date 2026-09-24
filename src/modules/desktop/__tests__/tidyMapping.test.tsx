import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import DesktopPanel, { validateTidyRows, type TidyMapRow } from "../DesktopPanel";
import {
  desktopLauncherStatus,
  desktopNoteList,
  desktopTidyPlan,
  desktopTidyStatus,
  hostConfigGet,
  hostConfigSet,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// D-29 B7/T-B7-16（09 §7.2）：整理映射的写口唯一在 DesktopPanel（config_schema 的 tidy_map
// 标 readOnly，设置中心不渲染）。保存必须走通用 host_config_set 且是**读-改-写**——
// 整替语义下漏带他键等于静默写缺省；保存后 refresh 重读盘，UI 回显持久化结果。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    desktopLauncherReindex: vi.fn(),
    desktopLauncherStatus: vi.fn(),
    desktopNoteAdd: vi.fn(),
    desktopNoteDone: vi.fn(),
    desktopNoteList: vi.fn(),
    desktopNoteRemove: vi.fn(),
    desktopTidyApply: vi.fn(),
    desktopTidyPlan: vi.fn(),
    desktopTidyRestore: vi.fn(),
    desktopTidyStatus: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

/** 模拟 ConfigStore 盘：set 落盘、get 读盘——persist 判据必须过盘一圈回 UI */
let disk: Record<string, unknown>;

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  disk = { tidy_map: null };
  vi.mocked(desktopTidyStatus).mockResolvedValue(false);
  vi.mocked(desktopTidyPlan).mockResolvedValue({ groups: [], total: 0 });
  vi.mocked(desktopLauncherStatus).mockResolvedValue([true, 4]);
  vi.mocked(desktopNoteList).mockResolvedValue([]);
  vi.mocked(hostConfigGet).mockImplementation(async () => structuredClone(disk));
  vi.mocked(hostConfigSet).mockImplementation(async (_module, values) => {
    disk = structuredClone(values as Record<string, unknown>);
  });
  vi.mocked(confirmAction).mockResolvedValue(true);
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container.remove();
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<DesktopPanel />);
  });
  await act(async () => {});
}

async function click(el: Element | undefined | null) {
  if (!el) throw new Error("目标控件未渲染");
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function inputByPlaceholder(prefix: string): HTMLInputElement {
  const el = [...container.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith(prefix),
  );
  if (!el) throw new Error(`placeholder 前缀「${prefix}」的输入框未渲染`);
  return el;
}

async function typeInto(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const buttonByText = (text: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);

describe("桌面整理分类映射编辑表（T-B7-16）", () => {
  it("desktopPanel_mappingTable_editPersists", async () => {
    await mount();
    // 初始态：盘上 tidy_map=null ⇒ 内置六类徽标、零编辑行
    expect(container.textContent).toContain("内置六类");
    expect(container.querySelector('input[placeholder^="类名"]')).toBeNull();

    await click(buttonByText("加行"));
    await typeInto(inputByPlaceholder("类名"), "设计稿");
    await typeInto(inputByPlaceholder("目标夹"), "D:\\Design");
    await typeInto(inputByPlaceholder("扩展名"), "psd,sketch");
    await click(buttonByText("保存映射"));

    // 写侧：通用 host_config_set（零新命令），线上形制 [类名, 目标夹, 扩展名[]]
    expect(hostConfigSet).toHaveBeenCalledWith("desktop", {
      tidy_map: { categories: [["设计稿", "D:\\Design", ["psd", "sketch"]]] },
    });
    // 持久回环：保存后 refresh 从盘重读，UI 回显自定义态（非仅本地乐观态）
    expect(container.textContent).toContain("自定义 1 行");
    expect(inputByPlaceholder("类名").value).toBe("设计稿");
    expect(vi.mocked(desktopTidyPlan).mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("desktopPanel_mappingTable_illegalFolder_blocksSave", async () => {
    await mount();
    await click(buttonByText("加行"));
    await typeInto(inputByPlaceholder("类名"), "文档");
    await typeInto(inputByPlaceholder("目标夹"), "Docs"); // 相对路径——红线臂
    await typeInto(inputByPlaceholder("扩展名"), "pdf");
    await click(buttonByText("保存映射"));
    expect(hostConfigSet).not.toHaveBeenCalled();
    expect(container.textContent).toContain("目标夹必须是带盘符的绝对路径");
  });
});

describe("validateTidyRows 预检（与 Rust tidy::validate 同纪律）", () => {
  const row = (over: Partial<TidyMapRow>): TidyMapRow => ({
    name: "x",
    folder: "D:\\d",
    exts: "psd",
    ...over,
  });
  it("每条红线各点名一次", () => {
    expect(validateTidyRows([row({ name: " " })])).toContain("分类名不得为空");
    expect(validateTidyRows([row({ name: "a/b" })])).toContain("路径分隔符");
    expect(
      validateTidyRows([row({}), row({ folder: "D:\\e", exts: "md" })]),
    ).toContain("分类名重复");
    expect(validateTidyRows([row({ folder: "\\\\nas\\s" })])).toContain("绝对路径");
    expect(validateTidyRows([row({ folder: 'D:\\a"b' })])).toContain("引号");
    expect(validateTidyRows([row({ exts: " , " })])).toContain("未声明任何扩展名");
    expect(validateTidyRows([row({ exts: "a.psd" })])).toContain("不带点");
    expect(validateTidyRows([row({})])).toBeNull();
  });
});
