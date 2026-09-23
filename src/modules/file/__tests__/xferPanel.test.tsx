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
// T-B6-9 追加：事件只作门铃（不读 payload 当数据）、方向徽标只贴跨边界行、
// resume 后旧幽灵行由后端收口（前端兜底口不再是唯一去幽灵路）。

const listenMock = vi.fn();

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

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
  delete (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__;
  listenMock.mockReset();
  listenMock.mockResolvedValue(() => {});
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
    // 全表滞后场景：file_ops_active 始终只回旧 Paused 行（模拟"入队节拍晚一拍"
    // 的窗口；真实幽灵行已由后端 T-B6-9 收口，见下方 resumeRebinds 用例），
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

describe("FilePanel 传输事件面诚实化（T-B6-9）", () => {
  it("filePanel_eventIsBellNotData_refetchesActiveOps：门铃响后以命令重取，事件载荷一个字都不进渲染", async () => {
    (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__ = {};
    vi.mocked(fileOpsActive).mockResolvedValue([opRow("live-1", { state: "running" })]);
    await mount();
    expect(listenMock).toHaveBeenCalledTimes(1);
    expect(listenMock.mock.calls[0][0]).toBe("nf:event");
    const bell = listenMock.mock.calls[0][1] as (e: unknown) => void;
    const before = vi.mocked(fileOpsActive).mock.calls.length;
    // 门铃"数据"全是假的：面板若读 payload 当事实源，ghost-9 / lie.bin 必上屏
    await act(async () => {
      bell({
        payload: {
          topic: "operation.progress",
          op_id: "ghost-9",
          kind: "copy",
          state: "running",
          current: "lie.bin",
          files_done: 7,
          files_total: 7,
          bytes_done: 999998,
          bytes_total: 999999,
          error: null,
          direction: "upload",
          resumable: "range",
          resumed_from: null,
        },
      });
    });
    // 正对照：确实重取了一次（mock invoke 计数 +1），而不是拿载荷凑渲染
    expect(vi.mocked(fileOpsActive).mock.calls.length).toBe(before + 1);
    expect(container.querySelector('[data-op-id="ghost-9"]')).toBeNull();
    expect(container.textContent).not.toContain("lie.bin");
    expect(container.querySelector('[data-op-id="live-1"]')).not.toBeNull();
  });

  it("filePanel_progressRowShowsDirectionBadge：上传/下载两徽标在位，本地复制不显示方向", async () => {
    vi.mocked(fileOpsActive).mockResolvedValue([
      opRow("up-1", { direction: "upload" }),
      opRow("down-1", { direction: "download", resumable: "range" }),
      opRow("loc-1", { direction: "local" }),
    ]);
    await mount();
    expect(container.querySelector('[data-op-id="up-1"]')?.textContent).toContain("上传");
    const down = container.querySelector('[data-op-id="down-1"]');
    expect(down?.textContent).toContain("下载");
    // 正对照：档位事实源在场时"支持断点续传"照旧上屏（徽标与档位两事实不互踩）
    expect(down?.textContent).toContain("支持断点续传");
    const loc = container.querySelector('[data-op-id="loc-1"]');
    expect(loc).not.toBeNull();
    // 本地不显示方向——防"处处贴方向"噪音
    expect(loc!.textContent).not.toMatch(/上传|下载/);
  });

  it("filePanel_resumeRebindsToNewOpIdAndDropsGhost：后端收口旧行后，前端重取即见旧行消失", async () => {
    // 与 T-B6-7 那枚的区别：本测模拟 T-B6-9 后端真收口——resume 之后的重取
    // 返回里旧行已不在场，面板不需要 xfer_status 兜底也得换绑成功。
    vi.mocked(fileOpsActive).mockResolvedValue([opRow("old-A", { state: "paused" })]);
    vi.mocked(fileOpResume).mockResolvedValue({ op_id: "new-B", previous_op_id: "old-A" });
    await mount();
    expect(container.querySelector('[data-op-id="old-A"]')).not.toBeNull();
    // 下一拍全表真相 = 只剩新行（latest 旧 Paused 行已被后端 resume 收口）
    vi.mocked(fileOpsActive).mockResolvedValue([opRow("new-B", { resumed_from: "old-A" })]);
    await click(buttonByText("恢复")!);
    expect(container.querySelector('[data-op-id="old-A"]')).toBeNull();
    const rowB = container.querySelector('[data-op-id="new-B"]');
    expect(rowB).not.toBeNull();
    expect(rowB!.textContent).toContain("续自 old-A");
    // 后端收口即无需定点兜底读口参与本用例（防"xferStatus 才是去幽灵路"的回退）
    expect(xferStatus).not.toHaveBeenCalled();
  });
});
