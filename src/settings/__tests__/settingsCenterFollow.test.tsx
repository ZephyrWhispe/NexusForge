import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import mainWorkbenchSrc from "../../windows/MainWorkbench.tsx?raw";
import SchemaForm from "../SchemaForm";
import { useSession } from "../../stores/session";
import { hostConfigGet, hostConfigSchema } from "../../ipc/client";

// D-29 B0/T-B0-4 回归：设置中心不再硬编码 clipboard，跟随"最近激活模块"。
// 正例走真实 store→SchemaForm 链（两模块各渲自身字段），
// 静态负例钉死 MainWorkbench 的 prop 传递形状。

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

const SCHEMAS: Record<string, { properties: Record<string, unknown> }> = {
  screenshot: {
    properties: {
      jpeg_quality: { type: "integer", title: "JPEG 质量（截图）", minimum: 1, maximum: 100 },
    },
  },
  proxy: {
    properties: {
      mode: { type: "string", title: "代理模式（proxy）" },
    },
  },
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(hostConfigSchema).mockImplementation(async (m) => SCHEMAS[m] ?? { properties: {} });
  vi.mocked(hostConfigGet).mockResolvedValue({});
});

afterEach(() => {
  act(() => root?.unmount());
  container.remove();
  vi.clearAllMocks();
});

async function mountSettings(moduleId: string) {
  await act(async () => {
    root = createRoot(container);
    root.render(<SchemaForm moduleId={moduleId} />);
  });
  await act(async () => {});
}

describe("设置中心跟随（T-B0-4）", () => {
  it("settingsCenter_followsActiveModule_screenshotAndProxy：切模块进设置，各渲自身表单字段", async () => {
    const s = useSession.getState();
    // 真实用户路径：选模块 → 点设置（__settings 不得覆写跟随目标）
    s.setActiveModule("screenshot");
    s.setActiveModule("__settings");
    expect(useSession.getState().lastModule).toBe("screenshot");
    await mountSettings(useSession.getState().lastModule);
    expect(hostConfigSchema).toHaveBeenCalledWith("screenshot");
    expect(container.textContent).toContain("JPEG 质量（截图）");
    expect(container.textContent).not.toContain("代理模式（proxy）");
    act(() => root.unmount());
    container = document.createElement("div");
    document.body.append(container);

    s.setActiveModule("proxy");
    s.setActiveModule("__settings");
    expect(useSession.getState().lastModule).toBe("proxy");
    await mountSettings(useSession.getState().lastModule);
    expect(hostConfigSchema).toHaveBeenCalledWith("proxy");
    expect(container.textContent).toContain("代理模式（proxy）");
    expect(container.textContent).not.toContain("JPEG 质量（截图）");

    // 负例臂：损坏/非模块 id 不覆写跟随目标
    useSession.getState().setActiveModule("corrupted-legacy-id");
    expect(useSession.getState().lastModule).toBe("proxy");
  });

  it("settingsCenter_moduleIdProp_noClipboardLiteral：MainWorkbench 只透传，不留 clipboard 字面量", () => {
    const src = String(mainWorkbenchSrc);
    expect(src).not.toContain('moduleId="clipboard"');
    expect(src).toContain("moduleId={settingsModule}");
    expect(src).toContain("key={settingsModule}");
    expect(src).toContain("s.lastModule");
  });
});
