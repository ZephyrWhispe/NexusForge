/**
 * D-42：`host.config_changed` 此前全仓零订阅（主题自 events.rs:47 起就登记在册）。
 * 别的写入方（另一窗口、托盘、自动化规则）改了本模块配置后，通用表单继续显示旧值，
 * 读起来像"改了没生效"。钉三件事：外部变更重取并显影／他模块不惊动／在途输入不被覆写。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SchemaForm from "../SchemaForm";
import { hostConfigGet, hostConfigSchema, hostConfigSet } from "../../ipc/client";

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

let eventCbs: ((e: { payload: unknown }) => void)[];
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_topic: string, cb: (e: { payload: unknown }) => void) => {
    eventCbs.push(cb);
    return () => {};
  }),
}));

const MODULE = "ocr";
const SCHEMA = {
  tesseract_exe: { type: "string", title: "Tesseract 可执行文件（绝对路径）", default: "" },
};
const EXTERNAL_VALUE = "C:\\tools\\tesseract-5\\tesseract.exe";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  eventCbs = [];
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(hostConfigSchema).mockResolvedValue({ properties: SCHEMA } as never);
  vi.mocked(hostConfigGet).mockResolvedValue({ tesseract_exe: "" } as never);
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => {
    try {
      root.unmount();
    } catch {
      /* 已卸载 */
    }
  });
  container.remove();
  document.body.replaceChildren();
  vi.clearAllMocks();
});

async function mountForm() {
  root = createRoot(container);
  await act(async () => {
    root.render(<SchemaForm moduleId={MODULE} />);
  });
  await act(async () => {});
  await act(async () => {});
  // 本表单的两条事件腿（config_rejected 显影＋config_changed 重取）并挂同一次订阅，
  // 注册仍要等动态 import 落地——发事件前不等齐就是正例假阴性
  await vi.waitFor(() => expect(eventCbs).toHaveLength(1));
}

async function fire(topic: string, payload: Record<string, unknown>) {
  await act(async () => {
    for (const cb of eventCbs) cb({ payload: { topic, payload } });
  });
}

const input = () => container.querySelector<HTMLInputElement>('input[type="text"]');

describe("SchemaForm 外部配置变更重取（D-42）", () => {
  it("configChanged_sameModule_refetchesAuthoritativeValue", async () => {
    await mountForm();
    const before = vi.mocked(hostConfigGet).mock.calls.length;
    // 盘上被别的写入方改了值：第二次读回新值
    vi.mocked(hostConfigGet).mockResolvedValueOnce({ tesseract_exe: EXTERNAL_VALUE } as never);
    await fire("host.config_changed", { module: MODULE });
    expect(vi.mocked(hostConfigGet).mock.calls.length).toBe(before + 1);
    expect(input()?.value).toBe(EXTERNAL_VALUE);
  });

  it("configChanged_otherModule_staysPut（负例）", async () => {
    await mountForm();
    const before = vi.mocked(hostConfigGet).mock.calls.length;
    await fire("host.config_changed", { module: "proxy" });
    expect(vi.mocked(hostConfigGet).mock.calls.length).toBe(before);
    // 正对照：表单在场，"没重取"不是因为整页没渲染
    expect(container.textContent).toContain("Tesseract 可执行文件（绝对路径）");
  });

  it("configChanged_whileEditingDoesNotClobberUnsavedKeys", async () => {
    await mountForm();
    const el = input();
    expect(el, "string 键渲染为 Input").not.toBeNull();
    // 敲进防抖窗口内（400ms 未落盘）：此刻重取会把用户正在输入的内容整表吞掉
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(el!, "C:\\我的\\路径");
      el!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const before = vi.mocked(hostConfigGet).mock.calls.length;
    await fire("host.config_changed", { module: MODULE });
    expect(vi.mocked(hostConfigGet).mock.calls.length).toBe(before);
    expect(el?.value).toBe("C:\\我的\\路径");
    // 用户自己的写仍然照常落盘（守卫只挡重取，不改保存语义）
    await act(async () => {
      await new Promise((r) => setTimeout(r, 450));
    });
    expect(hostConfigSet).toHaveBeenCalledWith(
      MODULE,
      expect.objectContaining({ tesseract_exe: "C:\\我的\\路径" }),
    );
  });
});
