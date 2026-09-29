/**
 * D-41 D5：`host.config_rejected` 的契约兑现——后端登记此主题时写明"设置面板须原样显示"，
 * 但全仓唯一监听者是截图面板且硬过滤 `module==="screenshot"`，其余模块的设置表单在
 * 拒收时全静默（`host_config_set` 的 promise 不 reject：写盘成功，拒收发生在写后的异步派发）。
 * 钉四件事：本模块拒收原文逐字上屏／他模块与非本域事件不污染／下次保存成功后撤除／卸载后不再触达。
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

/** nf:event 回调按订阅先后收集，用例里按需"发布"总线事件（同 shotUpload 夹具形制） */
let eventCbs: ((e: { payload: unknown }) => void)[];
let unlistenSpies: (() => void)[];
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_topic: string, cb: (e: { payload: unknown }) => void) => {
    eventCbs.push(cb);
    const spy = vi.fn();
    unlistenSpies.push(spy);
    return spy;
  }),
}));

const MODULE = "ocr";
const SCHEMA = {
  tesseract_exe: { type: "string", title: "Tesseract 可执行文件（绝对路径）", default: "" },
};
const REJECT_MSG =
  "Tesseract 可执行文件必须是绝对路径，当前值：tesseract.exe（不吃相对路径与裸程序名）";

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  eventCbs = [];
  unlistenSpies = [];
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(hostConfigSchema).mockResolvedValue({ properties: SCHEMA } as never);
  vi.mocked(hostConfigGet).mockResolvedValue({} as never);
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
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
  document.body.replaceChildren();
  vi.clearAllMocks();
});

async function mountForm() {
  await act(async () => {
    root = createRoot(container);
    root.render(<SchemaForm moduleId={MODULE} />);
  });
  // schema 读取与事件订阅各在一个微任务里落地
  await act(async () => {});
  await act(async () => {});
}

async function fire(topic: string, payload: Record<string, unknown>) {
  await act(async () => {
    for (const cb of eventCbs) cb({ payload: { topic, payload } });
  });
}

/** React 受控 Input 写值：走原生 value setter，否则 onChange 收不到变化 */
async function typeInput(input: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => {});
}

describe("SchemaForm 拒收显影（D-41 D5）", () => {
  it("configRejected_showsBackendVerbatim：本模块拒收→后端原话逐字上屏", async () => {
    await mountForm();
    await fire("host.config_rejected", { module: MODULE, error: REJECT_MSG });
    expect(container.textContent).toContain(REJECT_MSG);
  });

  it("configRejected_otherModuleAndOtherTopic_staySilent（负例）", async () => {
    await mountForm();
    await fire("host.config_rejected", { module: "proxy", error: "内核配置不合法" });
    expect(container.textContent).not.toContain("内核配置不合法");
    // 同模块的别的主题也不是本监听的猎物（契约面只有 config_rejected 一条错误腿）
    await fire("host.config_changed", { module: MODULE });
    // 正对照：表单本身在场，"没文案"不是因为整页没渲染
    expect(container.textContent).toContain("Tesseract 可执行文件（绝对路径）");
  });

  it("configRejected_clearedByNextSuccessfulSave：下次保存成功撤除，不陈旧双报", async () => {
    await mountForm();
    await fire("host.config_rejected", { module: MODULE, error: REJECT_MSG });
    expect(container.textContent).toContain(REJECT_MSG);
    const input = container.querySelector<HTMLInputElement>('input[type="text"]');
    expect(input, "string 键渲染为 Input").not.toBeNull();
    await typeInput(input!, "C:\\Program Files\\Tesseract-OCR\\tesseract.exe");
    await act(async () => {
      await new Promise((r) => setTimeout(r, 450));
    });
    expect(hostConfigSet).toHaveBeenCalledWith(
      MODULE,
      expect.objectContaining({ tesseract_exe: "C:\\Program Files\\Tesseract-OCR\\tesseract.exe" }),
    );
    expect(container.textContent).not.toContain(REJECT_MSG);
  });

  it("unmounted_formIgnoresEvent：卸载后取消订阅且事件不再触达（负例）", async () => {
    await mountForm();
    // D-42 起本表单的两条腿（config_rejected 显影＋config_changed 重取）并挂同一次订阅上，
    // 腿数增而订阅数不增
    expect(unlistenSpies).toHaveLength(1);
    await act(async () => {
      root.unmount();
    });
    expect(unlistenSpies[0]).toHaveBeenCalledTimes(1);
    await expect(fire("host.config_rejected", { module: MODULE, error: REJECT_MSG })).resolves.toBe(
      undefined,
    );
  });
});
