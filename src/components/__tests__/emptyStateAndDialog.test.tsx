import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Button, FluentProvider, webLightTheme } from "@fluentui/react-components";

import ConfirmDialogHost from "../ConfirmDialog";
import EmptyState from "../EmptyState";
import { useConfirmStore } from "../../stores/confirm";

// D-42 随批补齐两枚既有件的规范条款：
// ① EmptyState 4-4「空态必含为什么空＋第一步动作」——旧调用（仅 text）形态逐字保留，
//    新 hint/action 两槽可选（30+ 既有站点零改动）；
// ② 对话框按钮序 3 节「[取消] [保存/确认]，全应用无镜像」——主操作恒末位（＝视觉最右）。

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
});

afterEach(() => {
  act(() => root?.unmount());
  root = undefined!;
  container.remove();
  act(() => useConfirmStore.setState({ queue: [] }));
});

async function mount(el: React.ReactNode) {
  await act(async () => {
    if (!root) root = createRoot(container);
    root.render(<FluentProvider theme={webLightTheme}>{el}</FluentProvider>);
  });
}

describe("EmptyState 规范 4-4 扩展（D-42）", () => {
  it("emptyState_legacyFormUnchanged：仅 text 的旧形态不得多出节点", async () => {
    await mount(<EmptyState text="还没有收藏项" />);
    expect(container.textContent).toBe("还没有收藏项");
    expect(container.querySelectorAll("button")).toHaveLength(0);
  });

  it("emptyState_hintAndActionRender_sideBySide：why-empty 文案与第一步动作同屏", async () => {
    const onAction = () => {};
    await mount(
      <EmptyState
        text="没有匹配「kernel」的记录"
        hint="搜索为空不等于库为空"
        action={<Button onClick={onAction}>清空搜索</Button>}
      />,
    );
    expect(container.textContent).toContain("搜索为空不等于库为空");
    const button = container.querySelector("button");
    expect(button?.textContent).toBe("清空搜索");
  });

  it("emptyState_loadingKeepsStatusRole：加载态仍走 role=status（假空态守卫零退化）", async () => {
    await mount(<EmptyState text="不该出现" loading hint="不该出现" />);
    expect(container.textContent).toBe("加载中…");
    expect(container.querySelector("[role='status']")).not.toBeNull();
  });
});

describe("ConfirmDialog 按钮序（规范 3 节）", () => {
  it("confirmDialog_primaryIsLastButton：取消在前、主操作末位，且无镜像布局", async () => {
    let settled: boolean | null = null;
    void useConfirmStore.getState().ask({ title: "清空剪切板历史", impact: "将删除 128 条记录" }).then((ok) => {
      settled = ok;
    });
    await mount(<ConfirmDialogHost />);
    const buttons = [...document.querySelectorAll("button")];
    const labels = buttons.map((b) => b.textContent?.trim());
    expect(labels.slice(-2), "对话框动作区末两位须为 [取消, 确认执行]").toEqual(["取消", "确认执行"]);
    await act(async () => {
      buttons[buttons.length - 1]!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(settled).toBe(true);
  });
});
