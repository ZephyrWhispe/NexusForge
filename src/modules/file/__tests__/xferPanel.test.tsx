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
  fileOpResume,
  filePreview,
  fileSearch,
  fileList,
  xferStatus,
  type FileEntryDto,
  type OpProgressDto,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";

// D-29 B6/T-B6-7 回归：resume 返回新 op_id 被消费并以其为行新身份（承重③ UI 半）、
// Ask 冲突先上屏再携带决议重入队、错误行只呈现后端脱敏成品不掺原文、六态中文档全覆盖。

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
    fileOpResume: vi.fn(),
    xferStatus: vi.fn(),
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

/** 线上形状：state 是 snake_case（OpState 的 serde 真相），三新键 direction/resumable/resumed_from 在场 */
function opRow(opId: string, over: Partial<OpProgressDto> = {}): OpProgressDto {
  return {
    op_id: opId,
    kind: "copy",
    state: "running",
    current: "big.bin",
    files_done: 1,
    files_total: 2,
    bytes_done: 1024,
    bytes_total: 4096,
    error: null,
    direction: "local",
    resumable: null,
    resumed_from: null,
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

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileList).mockResolvedValue([entry("docs", true), entry("a.txt", false)]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([
    ["C:", "C:\\"],
    ["dir", "C:\\dir"],
  ]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileSearch).mockResolvedValue({ hits: [], degraded: false });
  vi.mocked(filePreview).mockResolvedValue({ kind: "unsupported", reason: "未配置" });
  vi.mocked(fileOpsActive).mockResolvedValue([]);
  vi.mocked(fileOpsPending).mockResolvedValue([]);
  vi.mocked(fileEnqueue).mockResolvedValue({ op_id: "op-new", conflicts: [] });
  vi.mocked(fileOpResume).mockResolvedValue({ op_id: "op-resumed", previous_op_id: "op-old" });
  vi.mocked(xferStatus).mockResolvedValue(opRow("op-resumed"));
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

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<FilePanel />);
  });
  await act(async () => {});
}

describe("FilePanel 传输状态类型化+断点真值+冲突消费（T-B6-7）", () => {
  it("xferPanel_resumeRow_rebindsPollingToNewOpId：resume 返回的新 op_id 成为行新身份（承重③ UI 半）", async () => {
    // 全表滞后场景：file_ops_active 始终只回旧 Paused 行（幽灵行清理归 T-B6-9），
    // 面板必须以 ResumeDto.op_id 经 xfer_status 定点兜底把续上行接上
    vi.mocked(fileOpsActive).mockResolvedValue([opRow("old-1", { state: "paused" })]);
    vi.mocked(fileOpResume).mockResolvedValue({
      op_id: "new-1",
      previous_op_id: "old-1",
    });
    vi.mocked(xferStatus).mockResolvedValue(
      opRow("new-1", { state: "running", resumed_from: "old-1", resumable: "range" }),
    );
    await mount();
    expect(buttonByText("恢复")).toBeDefined();
    await click(buttonByText("恢复")!);
    expect(fileOpResume).toHaveBeenCalledWith("old-1");
    expect(xferStatus).toHaveBeenCalledWith("new-1");
    // 新行上屏且链指回旧行（修前返回值被丢弃，本查询恒空）
    const newRow = container.querySelector('[data-op-id="new-1"]');
    expect(newRow).not.toBeNull();
    expect(newRow!.textContent).toContain("续自 old-1");
    // 续传档位只报对端声明：Range 正对照上屏
    expect(newRow!.textContent).toContain("支持断点续传");
  });

  it("xferPanel_asksConflictBeforeEnqueue_secondInvokeCarriesDecision：Ask 冲突先上屏，二次入队带具体决议", async () => {
    vi.mocked(fileEnqueue)
      .mockResolvedValueOnce({
        op_id: null,
        conflicts: [{ name: "a.txt", dst: "C:\\out\\a.txt" }],
      })
      .mockResolvedValue({ op_id: "op-2", conflicts: [] });
    await mount();
    await click(rowByText("a.txt")!);
    const dst = container.querySelector<HTMLInputElement>(
      'input[placeholder="目标目录（复制/移动用）"]',
    );
    expect(dst).not.toBeNull();
    await setInput(dst!, "C:\\out");
    await click(buttonByText("复制到…")!);
    // 第一次入队默认 ask：返回冲突清单而非静默执行（confirmAction 未被触碰＝没有"替你决议"）
    expect(vi.mocked(fileEnqueue).mock.calls[0][0].policy).toBe("ask");
    expect(confirmAction).not.toHaveBeenCalled();
    expect(container.textContent).toContain("1 个同名冲突");
    await click(buttonByText("重命名保留两者")!);
    // 二次入队逐字携带用户决议与原始源/目标（决议≌换策略重入队，禁面板自造第三引擎）
    const second = vi.mocked(fileEnqueue).mock.calls[1][0];
    expect(second.policy).toBe("rename");
    expect(second.srcs).toEqual(["C:\\dir\\a.txt"]);
    expect(second.dst).toBe("C:\\out");
    expect(notify).not.toHaveBeenCalledWith("error", expect.anything(), expect.anything());
  });

  it("xferPanel_errorLineIsMaskedNotRawBody：错误行只呈现脱敏成品，前端不掺响应原文", async () => {
    // 掩码唯一权威在后端 remote_error_message（T-B6-4 Rust 侧已钉）；本测验前端两面：
    // ①error 字段原样单行呈现（含"原因原文见应用日志"收尾），②除 error 外不渲染任何响应体
    const masked = "远端腿未接线：HTTP 500 …（原因原文见应用日志）";
    vi.mocked(fileOpsActive).mockResolvedValue([
      opRow("op-fail", { state: "failed", error: masked }),
    ]);
    await mount();
    const badge = container.querySelector('[data-op-id="op-fail"]');
    expect(badge).not.toBeNull();
    expect(badge!.textContent).toContain(masked);
    expect(badge!.textContent).toContain("已失败");
    // 面板不吞成因也不扩写：整行不含 raw body 特征（前端若拼 res.body 必现尖括号标签）
    expect(document.body.innerHTML).not.toContain("<html");
    expect(document.body.innerHTML).not.toContain("RAW_BODY_");
  });

  it("xferPanel_stateBadgeCoversAllSixStates：六态中文档全落位（snake_case 线上真相 + 受控词表）", async () => {
    // 修前判红双位：TS 字面量是 PascalCase 而线上是 snake_case ⇒ 活动/终态过滤恒空、
    // 队列整段隐形；且终态摘要裸报英文态名。本测以六枚中文档名逐一钉住。
    vi.mocked(fileOpsActive).mockResolvedValue([
      opRow("q", { state: "queued" }),
      opRow("r", { state: "running" }),
      opRow("p", { state: "paused" }),
      opRow("d", { state: "done" }),
      opRow("f", { state: "failed" }),
      opRow("c", { state: "canceled" }),
    ]);
    await mount();
    const text = container.textContent ?? "";
    for (const label of ["排队中", "进行中", "已暂停", "已完成", "已失败", "已取消"]) {
      expect(text).toContain(label);
    }
    // 正对照防空洞：三活动行 + 三终态徽标各自的 data-op-id 都在场（两过滤皆非恒空）
    for (const id of ["q", "r", "p", "d", "f", "c"]) {
      expect(container.querySelector(`[data-op-id="${id}"]`)).not.toBeNull();
    }
  });
});
