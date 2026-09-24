import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import FilePanel from "../FilePanel";
import {
  fileBreadcrumbs,
  fileDrives,
  fileEnqueue,
  fileOpsActive,
  fileOpsPending,
  fileList,
  type FileEntryDto,
  type NameFixItemDto,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";
import { useSession } from "../../../stores/session";

// D-29 B6/T-B7-26：远端名字符闸的前端分派面（六枚测之第六枚）。
// dispatchNameFix 四臂（nameFixPreview / nameFixRenamed / conflicts / done）
// 经 FilePanel 消费：Ask 先出预览对话框再带 name_fix:"auto_rename" 重投；
// AutoRename 回执 toast 逐行复述 原名→新名（静默改名与传败同罪）；
// Reject 臂=后端 Err 上屏且对话框零挂载；suggested=null 整批禁确认。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    fileList: vi.fn(),
    fileBreadcrumbs: vi.fn(),
    fileDrives: vi.fn(),
    fileEnqueue: vi.fn(),
    fileOpsActive: vi.fn(),
    fileOpsPending: vi.fn(),
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

function fixItem(over: Partial<NameFixItemDto> = {}): NameFixItemDto {
  return {
    name: "a?b.txt",
    bad: [{ char: "?", reason: "RFC3986：'?' 终结路径段并开启查询串" }],
    suggested: "a？b.txt",
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
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

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<FilePanel />);
  });
  await act(async () => {});
}

/** 浏览档选中 a.txt → 填目标 → 点「复制到…」，触发一次 fileEnqueue */
async function enqueueCopy() {
  await click(rowByText("a.txt")!);
  const dst = container.querySelector<HTMLInputElement>(
    'input[placeholder="目标目录（复制/移动用）"]',
  );
  expect(dst).not.toBeNull();
  await setInput(dst!, "C:\\out");
  await click(buttonByText("复制到…")!);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  delete (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileList).mockResolvedValue([entry("a.txt", false)]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([["C:", "C:\\"]]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileOpsActive).mockResolvedValue([]);
  vi.mocked(fileOpsPending).mockResolvedValue([]);
  vi.mocked(fileEnqueue).mockResolvedValue({ op_id: "op-x", conflicts: [], name_fix: [] });
  useSession.setState({ fileSubPanel: "browse" });
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

describe("FilePanel 远端名字符闸三档分派（T-B7-26 六枚测之 vitest 半）", () => {
  it("nameFixDialog_threePolicies_dispatch：Ask 预览→确认重投带 auto_rename", async () => {
    vi.mocked(fileEnqueue)
      .mockResolvedValueOnce({ op_id: null, conflicts: [], name_fix: [fixItem()] })
      .mockResolvedValueOnce({ op_id: "op-r", conflicts: [], name_fix: [] });
    await mount();
    await enqueueCopy();
    // Ask 臂：零入队回执 ⇒ 对话框上屏，原名与建议名逐字同屏（用户对着改名说 yes）
    const alert = document.querySelector('[role="dialog"]') ?? document.body;
    expect(alert.textContent).toContain("a?b.txt");
    expect(alert.textContent).toContain("a？b.txt");
    expect(alert.textContent).toContain("RFC3986");
    // 预览阶段不弹普通 toast、不误开冲突裁决（名闸优先于 conflicts 臂）
    expect(notify).not.toHaveBeenCalled();
    await click(buttonByText("按建议名继续")!);
    // 确认=换策略重入队（禁前端自造改名引擎）：第二次逐字携带原始 srcs/dst + 决议
    expect(fileEnqueue).toHaveBeenCalledTimes(2);
    const second = vi.mocked(fileEnqueue).mock.calls[1][0];
    expect(second.name_fix).toBe("auto_rename");
    expect(second.policy).toBe("ask");
    expect(second.srcs).toEqual(["C:\\dir\\a.txt"]);
    expect(second.dst).toBe("C:\\out");
    // 重投回执 done 臂后对话框卸载（按次挂载，旧预览行不定格）
    expect(buttonByText("按建议名继续")).toBeUndefined();
  });

  it("nameFixDialog_threePolicies_dispatch：AutoRename 回执 toast 复述原名→新名", async () => {
    // 配置档=auto_rename 时后端直接改名入队：op_id 有值 + name_fix 非空 = 复述臂
    vi.mocked(fileEnqueue).mockResolvedValue({
      op_id: "op-q",
      conflicts: [],
      name_fix: [fixItem(), fixItem({ name: "c*.dat", suggested: "c＊.dat" })],
    });
    await mount();
    await enqueueCopy();
    expect(notify).toHaveBeenCalledWith(
      "info",
      "远端名按冲突字符映射改名后入队",
      "a?b.txt → a？b.txt\nc*.dat → c＊.dat",
    );
    // 复述臂不进对话框（已经改完，无待裁决事项）
    expect(buttonByText("按建议名继续")).toBeUndefined();
  });

  it("nameFixDialog_threePolicies_dispatch：Reject 臂=后端 Err 上屏且对话框零挂载", async () => {
    vi.mocked(fileEnqueue).mockRejectedValue({
      kind: "Validation",
      data: {
        code: "FILE_REMOTE_010",
        message: "远端名含冲突字符且策略为拒绝：\"?\"（a?b.txt）",
      },
    });
    await mount();
    await enqueueCopy();
    const alert = document.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("FILE_REMOTE_010");
    expect(alert?.textContent).toContain("a?b.txt");
    // reject 无预览语义：不放行也不弹确认（错误即终音）
    expect(buttonByText("按建议名继续")).toBeUndefined();
    expect(notify).not.toHaveBeenCalled();
  });

  it("nameFixDialog_unfixableRow_disablesConfirm：映射不出干净新名整批禁确认", async () => {
    vi.mocked(fileEnqueue).mockResolvedValueOnce({
      op_id: null,
      conflicts: [],
      name_fix: [
        fixItem(),
        fixItem({ name: "e?.bin", bad: [{ char: "?", reason: "NUL 邻近禁" }], suggested: null }),
      ],
    });
    await mount();
    await enqueueCopy();
    const confirm = buttonByText("按建议名继续");
    expect(confirm).toBeDefined();
    expect(confirm!.disabled).toBe(true);
    // 无建议的行如实挂"（映射不出干净新名）"，前端不臆造第二名字
    expect(document.body.textContent).toContain("（映射不出干净新名）");
    expect(fileEnqueue).toHaveBeenCalledTimes(1);
  });
});
