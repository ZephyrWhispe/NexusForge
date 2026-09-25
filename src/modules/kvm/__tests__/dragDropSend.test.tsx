import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import DragDropSendDialog from "../DragDropSendDialog";
import type { KvmClientDevice } from "../dragDropFlow";

/** T-B8-1（D-33）确认对话框三枚（11 档 §8 回归列字面名）。独立挂载对话框组件，不牵 KvmPanel 数据面。 */

const devB: KvmClientDevice = { deviceId: "dev-b", deviceName: "台式机B" };
const devC: KvmClientDevice = { deviceId: "dev-c", deviceName: "笔记本C" };

let container: HTMLDivElement;
let root: Root;

// Fluent v9 Dialog 经 Portal 渲染到 document.body——断言面取全文档（T-B8-1 实发）
const byText = (sel: string, text: string) =>
  [...document.querySelectorAll(sel)].find(
    (el) => el.textContent?.includes(text),
  ) as HTMLElement | undefined;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("DragDropSendDialog (T-B8-1)", () => {
  it("kvmDragDropDialog_sendsSelectedDeviceOncePerFile", async () => {
    const sendOne = vi.fn(async () => {});
    const onDone = vi.fn();
    const onClose = vi.fn();
    const files = ["D:/tmp/报告 v2.docx", "C:/a.txt"];
    await act(async () => {
      root.render(
        <DragDropSendDialog
          files={files}
          devices={[devB, devC]}
          sendOne={sendOne}
          onDone={onDone}
          onClose={onClose}
        />,
      );
    });
    await act(async () => {});
    // 清单逐字在场（预览面）
    const items = [...document.querySelectorAll('[role="listitem"]')];
    expect(items.map((el) => el.textContent)).toEqual(files);
    // 换选目标设备＝笔记本C（默认首台之外的第二档，钉住选择槽真消费）
    const select = document.querySelector("select") as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent?.trim())).toEqual(["台式机B", "笔记本C"]);
    await act(async () => {
      select.value = "dev-c";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    const sendBtn = byText("button", "发送")!;
    await act(async () => {
      sendBtn.click();
    });
    expect(sendOne.mock.calls).toEqual([
      ["dev-c", files[0]],
      ["dev-c", files[1]],
    ]);
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(onDone.mock.calls[0][0]).toEqual([{ path: files[0] }, { path: files[1] }]);
  });

  it("kvmDragDropDialog_failureLinesSurfaceAndRecitePaths", async () => {
    const files = ["C:/a.txt", "C:/b.bin"];
    const sendOne = vi.fn(async (_d: string, path: string) => {
      if (path === files[1]) throw new Error("设备 dev-b 无活跃会话");
    });
    const onDone = vi.fn();
    await act(async () => {
      root.render(
        <DragDropSendDialog
          files={files}
          devices={[devB]}
          sendOne={sendOne}
          onDone={onDone}
          onClose={() => {}}
        />,
      );
    });
    await act(async () => {});
    await act(async () => {
      byText("button", "发送")!.click();
    });
    const [outcomes] = onDone.mock.calls[0] as [{ path: string; error?: string }[]];
    expect(outcomes).toEqual([
      { path: "C:/a.txt" },
      { path: "C:/b.bin", error: "设备 dev-b 无活跃会话" },
    ]);
    // 失败不中断后续文件（逐条腿语义）——两文件都被尝试过
    expect(sendOne).toHaveBeenCalledTimes(2);
  });

  it("kvmDragDropDialog_cancelSendsNothing", async () => {
    const sendOne = vi.fn(async () => {});
    const onDone = vi.fn();
    const onClose = vi.fn();
    await act(async () => {
      root.render(
        <DragDropSendDialog
          files={["C:/a.txt"]}
          devices={[devB]}
          sendOne={sendOne}
          onDone={onDone}
          onClose={onClose}
        />,
      );
    });
    await act(async () => {});
    await act(async () => {
      byText("button", "取消")!.click();
    });
    expect(sendOne).toHaveBeenCalledTimes(0);
    expect(onDone).toHaveBeenCalledTimes(0);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
