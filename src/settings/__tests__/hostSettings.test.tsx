import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import HostSettings from "../HostSettings";
import { hostConfigGet, hostConfigSchema, hostConfigSet } from "../../ipc/client";

// T-B7-11（09 §7.2）回归：宿主设置段（"tray"）走唯一 SchemaForm 引擎——
// 开关读回已落盘的 load_display，翻转即经 host_config_set("tray", …) 持久化。

vi.mock("../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../ipc/client")>();
  return {
    ...actual,
    hostConfigSchema: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});
vi.mock("../../ipc/env", () => ({ IN_TAURI: true }));
// D-41 D5：表单新增 nf:event 监听（host.config_rejected 显影），jsdom 无 Tauri 内部口
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
// D-41 D5：表单新增 nf:event 监听（host.config_rejected 显影），jsdom 无 Tauri 内部口
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

const TRAY_SCHEMA = {
  type: "object",
  properties: {
    load_display: {
      type: "boolean",
      title: "托盘显示资源负载",
      description: "标题显示「CPU x% · MEM y%」，≥5s 节流刷新；预警到达时自动让位",
      default: false,
    },
  },
};

let container: HTMLDivElement;
let root: Root;

/** SchemaForm 行内标题是 span（非 label for 形制），开关即 input[role=switch]；
 * tray 段 schema 恰一键 ⇒ 全集恰一开关，标题在场与开关在场同行断言 */
function switchInput(title: string): HTMLInputElement {
  expect(container.textContent, `应渲染标题「${title}」`).toContain(title);
  const boxes = container.querySelectorAll<HTMLInputElement>('input[role="switch"]');
  expect(boxes.length, "tray 段布尔键数 = 开关数").toBe(1);
  return boxes[0] as HTMLInputElement;
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<HostSettings />);
  });
  await act(async () => {});
}

async function toggle(input: HTMLInputElement) {
  await act(async () => {
    input.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(hostConfigSchema).mockImplementation(async (m) =>
    m === "tray" ? (TRAY_SCHEMA as never) : ({ properties: {} } as never),
  );
  vi.mocked(hostConfigGet).mockResolvedValue({} as never);
  vi.mocked(hostConfigSet).mockResolvedValue(undefined as never);
});

afterEach(() => {
  act(() => root?.unmount());
  container.remove();
  vi.clearAllMocks();
});

describe("宿主设置段（T-B7-11）", () => {
  it("hostSettings_trayLoadTogglePersists：schema 驱动渲染，翻转即写 tray 段且读回落盘值", async () => {
    await mount();
    expect(hostConfigSchema).toHaveBeenCalledWith("tray");
    // 缺盘值 → 回填 schema 默认 false（托盘常态改变须用户显式开）
    const input = switchInput("托盘显示资源负载");
    expect(input.checked).toBe(false);
    // 翻转 → 唯一写口 host_config_set("tray", {load_display:true})
    await toggle(input);
    expect(hostConfigSet).toHaveBeenCalledWith("tray", { load_display: true });
    expect(switchInput("托盘显示资源负载").checked).toBe(true);
    // 读回臂：模拟已落盘 true 的重开——开关必须呈 true（持久化闭环）
    act(() => root.unmount());
    container = document.createElement("div");
    document.body.append(container);
    vi.mocked(hostConfigGet).mockResolvedValue({ load_display: true } as never);
    await mount();
    expect(switchInput("托盘显示资源负载").checked).toBe(true);
  });
});
