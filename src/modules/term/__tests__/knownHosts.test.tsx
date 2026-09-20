import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import TerminalPanel from "../TerminalPanel";
import { termSessions, termSshForgetHost, termSshKnownHosts, termWslList } from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// D-29 B1/T-B1-8 回归：known_hosts 管理 Dialog 列表两列呈现；删除走
// confirmAction({danger:true, command:host})，host 串（含 "[h]:port" 复合形态）
// 原样回传——parse_host_port（commands/term.rs:195-203）依赖该形状，禁止前端拆解。

// xterm 桩：本用例组不建立会话（termSessions 返回 []），仅需模块可导入
vi.mock("@xterm/xterm", () => {
  class Terminal {
    element?: HTMLElement;
    loadAddon() {}
    open(el: HTMLElement) {
      this.element = el;
    }
    write() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
    onResize() {
      return { dispose() {} };
    }
  }
  return { Terminal };
});
vi.mock("@xterm/addon-fit", () => {
  class FitAddon {
    fit() {}
    dispose() {}
  }
  return { FitAddon };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    termSessions: vi.fn(),
    termWslList: vi.fn(),
    termSshKnownHosts: vi.fn(),
    termSshForgetHost: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const HOSTS = [
  { host: "example.com", fingerprint: "SHA256:aaa111" },
  { host: "[10.0.0.1]:2222", fingerprint: "SHA256:bbb222" },
];

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string, scope: ParentNode = container): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
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
    root.render(<TerminalPanel />);
  });
  await act(async () => {});
}

async function openDialog() {
  await click(buttonByText("已知主机")!);
  expect(termSshKnownHosts).toHaveBeenCalledTimes(1);
}

/** Dialog 表格行（按 HOSTS 顺序）：删除钮在行内 */
function deleteBtnForRow(idx: number): HTMLButtonElement {
  const rows = document.querySelectorAll("tbody tr");
  const btn = [...rows[idx].querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === "删除",
  );
  expect(btn).toBeDefined();
  return btn as HTMLButtonElement;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(termSessions).mockResolvedValue([]);
  vi.mocked(termWslList).mockResolvedValue([]);
  vi.mocked(termSshKnownHosts).mockResolvedValue(HOSTS);
  vi.mocked(termSshForgetHost).mockResolvedValue(true);
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

describe("TerminalPanel known_hosts 管理（T-B1-8）", () => {
  it("termKnownHosts_dialogListsFingerprints：打开即拉取，主机+指纹两列原样呈现", async () => {
    await mount();
    // 懒加载：未点入口不得提前 invoke
    expect(termSshKnownHosts).not.toHaveBeenCalled();
    await openDialog();
    const dialog = document.body;
    expect(dialog.textContent).toContain("已知主机（SSH 指纹管理）");
    // 复合形态 "[10.0.0.1]:2222" 原样展示（不拆成 host+端口两列语义）
    expect(dialog.textContent).toContain("example.com");
    expect(dialog.textContent).toContain("[10.0.0.1]:2222");
    expect(dialog.textContent).toContain("SHA256:aaa111");
    expect(dialog.textContent).toContain("SHA256:bbb222");
    expect(document.querySelectorAll("tbody tr")).toHaveLength(2);
  });

  it("termForgetHost_compositeHostPassthrough：复合 host 原样进确认框与 invoke，false 臂如实回报", async () => {
    await mount();
    await openDialog();
    await click(deleteBtnForRow(1)); // "[10.0.0.1]:2222" 行
    // 红线形状：danger 确认 + command 预览点名原样 host 串
    expect(confirmAction).toHaveBeenCalledWith(
      expect.objectContaining({ danger: true, command: "[10.0.0.1]:2222" }),
    );
    expect(termSshForgetHost).toHaveBeenCalledTimes(1);
    expect(termSshForgetHost).toHaveBeenCalledWith("[10.0.0.1]:2222");
    expect(container.textContent).toContain("已删除「[10.0.0.1]:2222」的指纹记录");
    // 删除后重拉列表（一次开框 + 一次删后刷新）
    expect(termSshKnownHosts).toHaveBeenCalledTimes(2);
    // false 臂：后端无"host 不存在"错误码，只能如实回报而非谎称已删除
    vi.mocked(termSshForgetHost).mockResolvedValueOnce(false);
    await click(deleteBtnForRow(0)); // "example.com" 行
    expect(termSshForgetHost).toHaveBeenLastCalledWith("example.com");
    expect(container.textContent).toContain("「example.com」未删除（可能已被移除）");
    expect(container.textContent).not.toContain("已删除「example.com」");
  });

  it("termForgetHost_cancelledZeroInvoke：确认框取消后删除零 invoke（红线负例）", async () => {
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await mount();
    await openDialog();
    await click(deleteBtnForRow(0));
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(termSshForgetHost).not.toHaveBeenCalled();
    // 取消不改列表：仍只有开框那一次拉取
    expect(termSshKnownHosts).toHaveBeenCalledTimes(1);
  });
});
