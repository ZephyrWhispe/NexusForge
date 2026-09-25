import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import KvmPanel from "../KvmPanel";
import { kvmSendFile, kvmSessionList, type SessionDto } from "../../../ipc/client";
import { notify } from "../../../stores/notifications";

// T-B8-1（D-33）接线层：窗口级 onDragDropEvent 订阅生命周期 + refuse/ready 两态分流
// （11 档 §8 回归列字面名）。事件形制为真（@tauri-apps/api DragDropEvent drop 臂），
// 真 drop 可达性属实启冒烟（D-33 验收）。

const dropHandlers: ((e: { payload: { type: string; paths: string[] } }) => void)[] = [];
const unlisten = vi.fn();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onDragDropEvent: vi.fn(async (h: (e: { payload: { type: string; paths: string[] } }) => void) => {
      dropHandlers.push(h);
      return unlisten;
    }),
  }),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    kvmDiscoveredPeers: vi.fn(async () => []),
    kvmPairedPeers: vi.fn(async () => [
      {
        device_id: "dev-b",
        device_name: "台式机B",
        fingerprint: `fp-dev-b-0123456789`,
        pubkey_b64: "cHVi",
        paired_at: 0,
      },
    ]),
    kvmSessionList: vi.fn(),
    kvmControlState: vi.fn(async () => ({ role: "idle" })),
    kvmEdgeMap: vi.fn(async () => ({})),
    kvmIssuePairCode: vi.fn(async () => ["ABCDEF", 60]),
    kvmSetEdgeMap: vi.fn(async () => {}),
    kvmSendFile: vi.fn(async () => {}),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const session = (id: string, name: string, role: "client" | "server"): SessionDto => ({
  device_id: id,
  device_name: name,
  role,
});

let container: HTMLDivElement;
let root: Root;

const settle = () => act(async () => {});

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  dropHandlers.length = 0;
  unlisten.mockClear();
  vi.mocked(kvmSendFile).mockClear();
  vi.mocked(notify).mockClear();
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe("KvmPanel drag-drop wiring (T-B8-1)", () => {
  it("kvmDragDrop_subscribesOnMount_unsubscribesOnUnmount", async () => {
    vi.mocked(kvmSessionList).mockResolvedValue([]);
    await act(async () => root.render(<KvmPanel />));
    await settle();
    await settle();
    expect(dropHandlers.length, "面板挂载期应订阅窗口拖放事件").toBe(1);

    // refuse 臂：无出站会话的 drop → 点名拒且不开对话框
    await act(async () => {
      dropHandlers[0]({ payload: { type: "drop", paths: ["C:/a.txt"] } });
    });
    await settle();
    expect(document.body.textContent).toContain("出站会话");
    expect(document.body.textContent).not.toContain("拖拽发送文件");

    await act(async () => root.unmount());
    expect(unlisten, "卸载必须退订（他视图零打扰的负证面）").toHaveBeenCalledTimes(1);
    root = createRoot(container); // 让 afterEach 的 unmount 合法
  });

  it("kvmDragDrop_dropWithSession_opensDialogAndAggregates", async () => {
    vi.mocked(kvmSessionList).mockResolvedValue([session("dev-b", "台式机B", "client")]);
    await act(async () => root.render(<KvmPanel />));
    await settle();
    await settle();
    expect(dropHandlers.length).toBe(1);

    await act(async () => {
      dropHandlers[0]({ payload: { type: "drop", paths: ["C:/a.txt", "C:/b.bin", ""] } });
    });
    await settle();
    // ready 臂：按次挂载的确认对话框在场，空路径已被裁决面剔除
    expect(document.body.textContent).toContain("拖拽发送文件");
    const items = [...document.querySelectorAll('[role="listitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(["C:/a.txt", "C:/b.bin"]);

    const sendBtn = [...document.querySelectorAll("button")].find((el) =>
      el.textContent?.includes("发送"),
    )!;
    await act(async () => {
      sendBtn.click();
    });
    expect(kvmSendFile).toHaveBeenCalledTimes(2);
    expect(vi.mocked(kvmSendFile).mock.calls).toEqual([
      ["dev-b", "C:/a.txt"],
      ["dev-b", "C:/b.bin"],
    ]);
    expect(notify).toHaveBeenCalledWith(
      "success",
      "拖拽发送：2 个文件已开始",
      "进度与回执经会话事件回报",
    );
    // onDone 后对话框收挂（按次挂载生命周期）
    expect(document.body.textContent).not.toContain("拖拽发送文件");
  });
});
