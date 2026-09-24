import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import FilePanel from "../FilePanel";
import {
  fileBreadcrumbs,
  fileDrives,
  filePreview,
  fileSearch,
  fileList,
  type FileEntryDto,
  type PreviewDto,
  type SearchResultDto,
} from "../../../ipc/client";
import { useSession } from "../../../stores/session";

// D-29 B1/T-B1-4 回归：全局搜索必须显式携带降级标注（后端无持久索引，USN 端口
// 缺失即 walkdir，不标注就是在谎称全量），且空查询/加载中不得残留上一轮结果；
// 双击文件行开预览分栏（今天 openEntry 对文件什么都不做）。
// T-B7-27 七档全拆：搜索面判据逐字未动，只加切档步骤（搜索框/结果随搜索档挪出
// browse，纯挪移零裁减）；预览分栏是文件/搜索两档共用的同一份状态。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    fileList: vi.fn(),
    fileBreadcrumbs: vi.fn(),
    fileDrives: vi.fn(),
    fileSearch: vi.fn(),
    filePreview: vi.fn(),
    fileOpsActive: vi.fn(),
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

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}
function searchInput(): HTMLInputElement | null {
  return document.querySelector<HTMLInputElement>('input[aria-label="全局搜索关键词"]');
}
function rowByText(text: string): Element | undefined {
  return [...container.querySelectorAll("tr")].find((r) => r.textContent?.includes(text));
}

async function typeInto(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function dblClick(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
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
    entry("b.bin", false),
  ]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([
    ["C:", "C:\\"],
    ["dir", "C:\\dir"],
  ]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileSearch).mockResolvedValue({ hits: [], degraded: false });
  vi.mocked(filePreview).mockResolvedValue({ kind: "unsupported", reason: "未配置" });
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

describe("FilePanel 搜索 + 预览分栏（T-B1-4）", () => {
  it("fileSearch_degradedFlag_shownAndHitsRendered：降级横幅逐字渲染且命中可见；非降级无横幅", async () => {
    vi.mocked(fileSearch).mockResolvedValue({
      hits: [{ path: "C:\\Users\\me\\notes.txt", score: 9 }],
      degraded: true,
    });
    useSession.setState({ fileSubPanel: "search" }); // T-B7-27：搜索面随搜索档挪出 browse
    await mount();
    await typeInto(searchInput()!, "notes");
    await click(buttonByText("搜索")!);
    expect(fileSearch).toHaveBeenCalledWith("notes", 50, null);
    expect(container.textContent).toContain("索引降级：本次为目录遍历（深度≤6）");
    expect(container.textContent).toContain("C:\\Users\\me\\notes.txt");
    // 正对照：USN 索引命中（degraded=false）不得虚挂降级横幅
    vi.mocked(fileSearch).mockResolvedValue({
      hits: [{ path: "C:\\Users\\me\\notes.txt", score: 9 }],
      degraded: false,
    });
    await click(buttonByText("搜索")!);
    expect(container.textContent).toContain("C:\\Users\\me\\notes.txt");
    expect(container.textContent).not.toContain("索引降级");
  });

  it("fileSearch_emptyOrLoading_noStalePane：加载态与空查询均不残留上一轮命中", async () => {
    vi.mocked(fileSearch).mockResolvedValue({
      hits: [{ path: "C:\\dir\\old-hit.txt", score: 5 }],
      degraded: false,
    });
    useSession.setState({ fileSubPanel: "search" }); // T-B7-27：同上
    await mount();
    await typeInto(searchInput()!, "old");
    await click(buttonByText("搜索")!);
    expect(container.textContent).toContain("C:\\dir\\old-hit.txt");

    // 第二轮在途：旧命中必须先撤下，加载态替位
    let resolveSearch!: (v: SearchResultDto) => void;
    vi.mocked(fileSearch).mockReturnValueOnce(
      new Promise<SearchResultDto>((r) => {
        resolveSearch = r;
      }),
    );
    await typeInto(searchInput()!, "new");
    await click(buttonByText("搜索")!);
    expect(container.textContent).toContain("正在全局搜索");
    expect(container.textContent).not.toContain("C:\\dir\\old-hit.txt");
    resolveSearch({ hits: [], degraded: true });
    await act(async () => {});
    expect(container.textContent).toContain("索引降级：本次为目录遍历（深度≤6）");
    expect(container.textContent).toContain("没有名称匹配「new」的命中");

    // 清空查询：结果区整体撤下，且不为空串多发一次 IPC
    await typeInto(searchInput()!, "");
    expect(container.textContent).not.toContain("没有名称匹配");
    expect(container.textContent).not.toContain("索引降级");
    expect(fileSearch).toHaveBeenCalledTimes(2);
  });

  it("filePreview_textAndUnsupported_kindsRender：双击文件行开预览，四形态按判别渲染", async () => {
    const textP: PreviewDto = { kind: "text", content: "第一行内容\n第二行", truncated: true };
    vi.mocked(filePreview).mockResolvedValue(textP);
    await mount();
    await dblClick(rowByText("a.txt")!);
    expect(filePreview).toHaveBeenCalledWith("C:\\dir\\a.txt");
    expect(container.textContent).toContain("第一行内容");
    // 截断提示如实且不承诺可调
    expect(container.textContent).toContain("仅显示开头部分（截断限额由服务端固定）");

    vi.mocked(filePreview).mockResolvedValue({
      kind: "unsupported",
      reason: "二进制文件不支持文本预览",
    });
    await dblClick(rowByText("b.bin")!);
    expect(container.textContent).toContain("无法预览：二进制文件不支持文本预览");
    // 切换目标不得残留上一文件内容（防说谎分栏）
    expect(container.textContent).not.toContain("第一行内容");
    // 目录行双击仍是导航而非预览
    await dblClick(rowByText("docs")!);
    expect(filePreview).toHaveBeenCalledTimes(2);
  });
});
