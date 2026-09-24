import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import TerminalPanel from "../TerminalPanel";
import { termSessions, termSshConnect, termWslList } from "../../../ipc/client";

// D-29 B7/T-B7-4 回归：ProxyJump 跳板区默认收起——收起态连跳字段都不渲染
// （展开前零采集），展开后单跳表单随 termSshConnect 的 jump 参数外发。

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
    termSshConnect: vi.fn(),
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

function inputByPlaceholder(prefix: string): HTMLInputElement | undefined {
  return [...document.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith(prefix),
  );
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<TerminalPanel />);
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(termSessions).mockResolvedValue([]);
  vi.mocked(termWslList).mockResolvedValue([]);
  vi.mocked(termSshConnect).mockResolvedValue({
    id: "s1",
    kind: { kind: "local" },
    title: "t",
    cols: 100,
    rows: 26,
    alive: true,
  } as never);
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

describe("TerminalPanel ProxyJump 跳板区（T-B7-4）", () => {
  it("connectDialog_jumpSection_collapsedByDefault：收起态零跳字段；展开后单跳随 jump 参数外发；再收起回落 jump:null", async () => {
    await mount();
    const toggle = buttonByText("ProxyJump 跳板");
    expect(toggle, "缺跳板区入口钮").toBeDefined();
    // 默认收起：机检面=任何跳字段都不在 DOM（展开前零采集）
    expect(inputByPlaceholder("跳板主机")).toBeUndefined();
    expect(inputByPlaceholder("跳板端口")).toBeUndefined();
    expect(inputByPlaceholder("跳板密码")).toBeUndefined();
    // 展开：四枚字段入 DOM
    await click(toggle!);
    expect(buttonByText("收起 ProxyJump 跳板")).toBeDefined();
    const hopHost = inputByPlaceholder("跳板主机");
    expect(hopHost, "展开后应现跳板主机输入框").toBeDefined();
    setInputValue(hopHost!, "j1.example");
    setInputValue(inputByPlaceholder("跳板端口")!, "2201");
    setInputValue(inputByPlaceholder("跳板用户")!, "jumpuser");
    setInputValue(inputByPlaceholder("跳板密码")!, "jp");
    // 目标腿照常填（jump 与目标凭据互不串台）
    setInputValue(inputByPlaceholder("SSH 主机")!, "target.example");
    await click(buttonByText("SSH 连接")!);
    expect(termSshConnect).toHaveBeenCalledTimes(1);
    expect(termSshConnect).toHaveBeenCalledWith(
      expect.objectContaining({
        host: "target.example",
        jump: {
          host: "j1.example",
          port: 2201,
          user: "jumpuser",
          auth: { kind: "password", password: "jp" },
          via: null,
        },
      }),
    );
    // 正对照：再收起 = jump 回落 null（后端 jump=None 直连腿逐字不变）
    await click(buttonByText("收起 ProxyJump 跳板")!);
    expect(inputByPlaceholder("跳板主机")).toBeUndefined();
    await click(buttonByText("SSH 连接")!);
    expect(termSshConnect).toHaveBeenCalledTimes(2);
    expect(vi.mocked(termSshConnect).mock.calls[1][0].jump).toBeNull();
  });
});
