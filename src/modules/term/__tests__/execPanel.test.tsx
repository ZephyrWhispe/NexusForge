import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import TerminalPanel from "../TerminalPanel";
import { termSessions, termSshExec, termWslList } from "../../../ipc/client";

// D-29 B7/T-B7-2 回归：一次性远端命令 exec 面板——三分区（stdout/stderr/
// 退出码）呈现，exit≠0 或超时走 danger 徽标（data-exit-tone 机检面），
// 结果永不回灌终端回显。

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
    termSshExec: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

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

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<TerminalPanel />);
  });
  await act(async () => {});
}

async function openExecDialog() {
  await click(buttonByText("一次性远端命令")!);
  return [...document.querySelectorAll("input")].find(
    (i) => i.placeholder === "非交互命令，如 hostname -s",
  )!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(termSessions).mockResolvedValue([]);
  vi.mocked(termWslList).mockResolvedValue([]);
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

describe("TerminalPanel exec 面板（T-B7-2）", () => {
  it("execPanel_failedExitShowsRed：exit≠0 时 stderr 区可见且徽标点名退出码走 danger", async () => {
    vi.mocked(termSshExec).mockResolvedValue({
      exit_code: 3,
      stdout: "partial-out",
      stderr: "boom: no such file",
      timed_out: false,
    });
    await mount();
    // 懒打开：未点入口不得 invoke
    expect(termSshExec).not.toHaveBeenCalled();
    const cmdInput = await openExecDialog();
    // 执行钮空命令禁用（前端预筛，后端 BadParam 仍有一道）
    expect(buttonByText("执行")!.disabled).toBe(true);
    setInputValue(cmdInput, "cat missing.txt");
    await click(buttonByText("执行")!);
    expect(termSshExec).toHaveBeenCalledTimes(1);
    expect(termSshExec).toHaveBeenCalledWith(
      expect.objectContaining({ host: "", port: 22, user: "root" }),
      "cat missing.txt",
    );
    const badge = document.querySelector("[data-exit-tone]");
    expect(badge).not.toBeNull();
    expect(badge!.getAttribute("data-exit-tone")).toBe("danger");
    expect(badge!.textContent).toBe("退出码 3");
    expect(document.body.textContent).toContain("boom: no such file");
    expect(document.body.textContent).toContain("partial-out");
    // 正对照：exit=0 走 ok 徽标——danger 不是无条件默认值
    vi.mocked(termSshExec).mockResolvedValueOnce({
      exit_code: 0,
      stdout: "fine",
      stderr: "",
      timed_out: false,
    });
    await click(buttonByText("执行")!);
    expect(document.querySelector("[data-exit-tone]")!.getAttribute("data-exit-tone")).toBe("ok");
    expect(document.body.textContent).toContain("退出码 0");
    // 超时臂：timed_out 是唯一终态证据，未获退出码不并进 0
    vi.mocked(termSshExec).mockResolvedValueOnce({
      exit_code: null,
      stdout: "slow",
      stderr: "",
      timed_out: true,
    });
    await click(buttonByText("执行")!);
    const tb = document.querySelector("[data-exit-tone]")!;
    expect(tb.getAttribute("data-exit-tone")).toBe("danger");
    expect(tb.textContent).toContain("超时");
  });
});
