import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ForwardSection from "../ForwardSection";
import {
  termForwardList,
  termForwardOpen,
  type ForwardSpecDto,
  type TermSessionDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-5 回归（红线批：端口暴露）：转发管理表逐行真 state 徽标，
// **端口占用显示被拒行而非静默空表**——bind 撞车后端回 Refused 并进表，
// 面板必须把它渲染出来（静默换端口/静默吞失败=用户以为转对了）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    termForwardList: vi.fn(),
    termForwardOpen: vi.fn(),
    termForwardClose: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

let container: HTMLDivElement;
let root: Root;

const sshSession: TermSessionDto = {
  id: "s1",
  kind: { kind: "ssh", host: "h.example", port: 22, user: "root" },
  title: "root@h.example",
  alive: true,
  cols: 80,
  rows: 24,
};

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<ForwardSection sessions={[sshSession]} activeId="s1" />);
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
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

describe("转发管理表 state 徽标（T-B7-5）", () => {
  it("forwardTable_stateBadgesRenderNotEmptyOnFailure：被拒行渲染徽标+原因，表非空", async () => {
    const rows: ForwardSpecDto[] = [
      {
        id: "f-listen",
        kind: { local: { listen_port: 8080, dest_host: "localhost", dest_port: 80 } },
        state: { listening: { bound_port: 8080 } },
      },
      {
        id: "f-refused",
        kind: {
          remote: {
            bind: "127.0.0.1",
            listen_port: 14321,
            dest_host: "localhost",
            dest_port: 80,
          },
        },
        state: { refused: { reason: "bind 127.0.0.1:14321 失败——不自动换端口" } },
      },
    ];
    vi.mocked(termForwardList).mockResolvedValue(rows);
    await mount();
    expect(termForwardList).toHaveBeenCalledWith("s1");
    // 徽标逐行真 state：listening 与 refused 都在 DOM
    const listening = document.querySelector('[data-fwd-state="listening"]');
    const refused = document.querySelector('[data-fwd-state="refused"]');
    expect(listening, "Listening 徽标必须渲染").not.toBeNull();
    expect(refused, "Refused 徽标必须渲染（失败不得静默吞）").not.toBeNull();
    expect(listening!.textContent).toContain("监听 :8080");
    expect(refused!.textContent).toContain("被拒");
    // 被拒原因原文可见——用户据此知道是哪条腿撞了端口
    expect(document.body.textContent).toContain("不自动换端口");
    // 表非空：两行都在（失败≠空表的出路）
    expect(document.querySelectorAll("[data-fwd-row]")).toHaveLength(2);
  });

  it("forwardTable_openReturnsRefusedRow_repolledNotEmpty：open 回被拒后重取列表仍见被拒行", async () => {
    // 正对照：空列表 → "暂无转发"；随后一次 open 撞端口回 Refused 并进表，
    // 面板重取（termForwardList 第二次含被拒行）必须渲染出徽标而非停在空表
    vi.mocked(termForwardList)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([
        {
          id: "f2",
          kind: { local: { listen_port: 9999, dest_host: "localhost", dest_port: 3 } },
          state: { refused: { reason: "bind 失败：端口占用" } },
        },
      ]);
    vi.mocked(termForwardOpen).mockResolvedValue({
      id: "f2",
      kind: { local: { listen_port: 9999, dest_host: "localhost", dest_port: 3 } },
      state: { refused: { reason: "bind 失败：端口占用" } },
    });
    await mount();
    expect(document.body.textContent).toContain("暂无转发");
    // 填表并新增（listen=9999、dest 主机 3 端口）
    const inputs = [...document.querySelectorAll("input")];
    const listen = inputs.find((i) => i.placeholder === "监听端口")!;
    const destHost = inputs.find((i) => i.placeholder === "目标主机")!;
    const destPort = inputs.find((i) => i.placeholder === "目标端口")!;
    const setVal = (el: HTMLInputElement, v: string) => {
      const setter = Object.getOwnPropertyDescriptor(
        window.HTMLInputElement.prototype,
        "value",
      )!.set!;
      setter.call(el, v);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    };
    await act(async () => {
      setVal(listen, "9999");
      setVal(destHost, "localhost");
      setVal(destPort, "3");
    });
    const addBtn = [...document.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "新增转发",
    )!;
    expect(addBtn.disabled, "填齐后新增钮应解禁").toBe(false);
    await act(async () => {
      addBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await act(async () => {});
    expect(termForwardOpen).toHaveBeenCalledWith("s1", {
      local: { listen_port: 9999, dest_host: "localhost", dest_port: 3 },
    });
    const refused = document.querySelector('[data-fwd-state="refused"]');
    expect(refused, "撞端口后重取列表须渲染被拒行，不得停在空表").not.toBeNull();
    expect(document.body.textContent).toContain("端口占用");
  });
});
