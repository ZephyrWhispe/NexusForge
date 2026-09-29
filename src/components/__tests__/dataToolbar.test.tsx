import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Button, FluentProvider, Input, webLightTheme } from "@fluentui/react-components";

import DataToolbar from "../DataToolbar";

// D-42 组件族回归（00-ui-layout-spec 1/5 节）。分区次序＝本档判据：
// 左区 搜索→过滤→排序，右区 批量→主按钮且主按钮恒末位（"最右"在 DOM 上的等价形）。

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
});

async function mount(el: React.ReactNode) {
  await act(async () => {
    if (!root) root = createRoot(container);
    root.render(<FluentProvider theme={webLightTheme}>{el}</FluentProvider>);
  });
}

function toolbar(): HTMLElement {
  const el = container.querySelector('[role="toolbar"]');
  if (!el) throw new Error("DataToolbar 未渲染 role=\"toolbar\"");
  return el as HTMLElement;
}

describe("DataToolbar（D-42）", () => {
  it("dataToolbar_partitionOrder：左区搜索→过滤→排序，右区批量→主按钮末位", async () => {
    await mount(
      <DataToolbar
        search={<Input aria-label="搜索" placeholder="搜索" />}
        filters={<Button onClick={vi.fn()}>代码</Button>}
        sort={<Button onClick={vi.fn()}>排序</Button>}
        bulk={<Button onClick={vi.fn()}>已选 2</Button>}
        primary={<Button appearance="primary" onClick={vi.fn()}>新建</Button>}
      />,
    );
    const left = toolbar().firstElementChild!;
    const texts = [...left.querySelectorAll("button")].map((b) => b.textContent?.trim());
    expect(texts, "过滤芯片在排序之前").toEqual(["代码", "排序"]);
    expect(left.querySelector("input"), "搜索框是左区首件").not.toBeNull();

    const kids = [...toolbar().children];
    expect(kids[kids.length - 1]!.textContent).toBe("新建");
    expect(kids[kids.length - 1]!.querySelector("button")!.className).not.toBe("");
    // 主按钮前一位是批量区（右区次序＝批量→主按钮），且两者都在左区分隔件之后
    const order = kids.map((el) => el.textContent?.trim());
    expect(order.indexOf("已选 2")).toBeGreaterThan(order.indexOf("排序"));
  });

  it("dataToolbar_noWrapAndStickyHooks_present", async () => {
    await mount(<DataToolbar search={<Button onClick={vi.fn()}>仅搜索</Button>} />);
    // 缺省分区不渲染空壳件（无 filters/primary 时不得留下空 div 撑出假间距）
    const buttons = [...toolbar().querySelectorAll("button")];
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(["仅搜索"]);
  });

  it("dataToolbar_primaryNeverFoldsIsNotAHiddenCapability：主按钮在场即恒为末位可点子节点（10-15 折叠次序）", async () => {
    const onClick = vi.fn();
    await mount(
      <DataToolbar
        search={<Input aria-label="搜索" />}
        primary={
          <Button appearance="primary" onClick={onClick}>
            导入
          </Button>
        }
      />,
    );
    const last = toolbar().lastElementChild!;
    expect(last.textContent).toBe("导入");
    const button = last.querySelector("button");
    expect(button).not.toBeNull();
    expect(button!.disabled, "主按钮不得以 disabled 形式伪装折叠").toBe(false);
    await act(async () => {
      button!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
