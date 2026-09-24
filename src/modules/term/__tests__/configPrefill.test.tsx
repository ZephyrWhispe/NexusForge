import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import TerminalPanel from "../TerminalPanel";
import { termSessions, termSshConfigHosts, termWslList } from "../../../ipc/client";

// D-29 B7/T-B7-3 回归：~/.ssh/config 只读导入下拉——懒载（未展开零后端读）、
// 选中即预填表单（identity_file 顺带切密钥模式）、预填后用户仍可逐格覆盖。

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
    termSshConfigHosts: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

let container: HTMLDivElement;
let root: Root;

async function step() {
  await act(async () => {});
}

async function pointerClick(el: HTMLElement) {
  // Fluent v9 下拉只在真实指针序列下展开（同 schemaForm 测试先例）
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      el.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
  await act(async () => {
    await new Promise((r) => setTimeout(r, 5));
  });
}

function setInputValue(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function inputByPlaceholder(ph: string): HTMLInputElement {
  const el = [...document.querySelectorAll("input")].find((i) => i.placeholder === ph);
  if (!el)
    throw new Error(
      `缺输入框：${ph}；现有 ${[...document.querySelectorAll("input")]
        .map((i) => `${i.placeholder}/${i.type}`)
        .join(" | ")}`,
    );
  return el as HTMLInputElement;
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<TerminalPanel />);
  });
  await step();
}

async function openCfgDropdown() {
  const trigger = [...document.querySelectorAll<HTMLElement>('[role="combobox"]')].find(
    (t) => t.textContent?.includes("从 ~/.ssh/config 导入"),
  );
  if (!trigger) throw new Error("缺 config 导入下拉触发钮");
  await pointerClick(trigger);
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

describe("TerminalPanel config 只读导入预填（T-B7-3）", () => {
  it("importPrefill_fillsForm_userCanOverride：展开懒载→选中预填→用户可逐格改回", async () => {
    vi.mocked(termSshConfigHosts).mockResolvedValue([
      {
        alias: "web",
        host_name: "web.example.com",
        user: "deploy",
        port: 2222,
        identity_file: "~/.ssh/id_web",
      },
      { alias: "db", host_name: null, user: null, port: null, identity_file: null },
    ]);
    await mount();
    // 懒载：未展开下拉前零后端读（只读面也不白读）
    expect(termSshConfigHosts).not.toHaveBeenCalled();
    await openCfgDropdown();
    expect(termSshConfigHosts).toHaveBeenCalledTimes(1);
    // 选中 web：四字段+别名回落全预填，identity_file 顺带切到密钥模式
    const opt = [...document.querySelectorAll<HTMLElement>(".fui-Option")].find((o) =>
      o.textContent?.includes("web"),
    );
    expect(opt, "展开后应见 web 选项").toBeDefined();
    await pointerClick(opt!);
    expect(inputByPlaceholder("SSH 主机").value).toBe("web.example.com");
    expect(inputByPlaceholder("用户").value).toBe("deploy");
    expect(inputByPlaceholder("端口").value).toBe("2222");
    // JSX 属性串不解转义：占位符里是字面双反斜杠
    expect(inputByPlaceholder("私钥路径（如 C:\\\\Users\\\\me\\\\.ssh\\\\id_ed25519）").value).toBe(
      "~/.ssh/id_web",
    );
    expect(document.body.textContent).toContain("已导入「web」（仍可修改）");
    // 正对照（覆盖性）：预填不是锁填——用户改主机名即刻生效
    setInputValue(inputByPlaceholder("SSH 主机"), "manual.example");
    await step();
    expect(inputByPlaceholder("SSH 主机").value).toBe("manual.example");
    // 再展开用缓存：懒载只读一次
    await openCfgDropdown();
    expect(termSshConfigHosts).toHaveBeenCalledTimes(1);
    // 纯别名条目（host_name=null）回落 alias 本身
    const optDb = [...document.querySelectorAll<HTMLElement>(".fui-Option")].find((o) =>
      o.textContent?.includes("db"),
    );
    expect(optDb, "展开后应见 db 选项").toBeDefined();
    await pointerClick(optDb!);
    expect(inputByPlaceholder("SSH 主机").value).toBe("db");
  });
});
