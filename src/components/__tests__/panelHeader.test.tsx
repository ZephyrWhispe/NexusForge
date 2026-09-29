import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Button, FluentProvider, webLightTheme } from "@fluentui/react-components";

import PanelHeader from "../PanelHeader";

// D-42 组件族回归（00-ui-layout-spec 1/3 节）。
// jsdom 下 griffel 哈希类不注入可查询样式，故判据取"结构次序"这一可断言的本质：
// 规范说"主操作恒最右"，落到 DOM 就是 actions 是行内最后一个子节点；
// 真实像素高 48 属实启骨架走查项（D-42 验收段），不在此伪称。

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

describe("PanelHeader（D-42）", () => {
  it("panelHeader_actionsAreLastChild_nodesktopOrderIsAssertable", async () => {
    await mount(
      <PanelHeader
        title="剪切板"
        context="共 128 条 · 敏感库 3 条"
        actions={<Button onClick={vi.fn()}>新建</Button>}
      />,
    );
    const header = container.querySelector("header");
    expect(header).not.toBeNull();
    expect(header!.textContent).toContain("剪切板");
    expect(header!.textContent).toContain("共 128 条 · 敏感库 3 条");
    // 主操作恒最右＝行内最后一个子节点（弹性 spacer 之前不留散落件）
    expect(header!.lastElementChild!.tagName).toBe("BUTTON");
    expect(header!.lastElementChild!.textContent).toBe("新建");
  });

  it("panelHeader_contextOmitted_rendersNoStrayNode", async () => {
    await mount(<PanelHeader title="保险库" />);
    const header = container.querySelector("header");
    expect(header!.textContent).toBe("保险库");
    // 无 actions 时末位是弹性 spacer（不得把标题挤成末位＝视觉末端错位）
    expect(header!.lastElementChild!.tagName).not.toBe("BUTTON");
  });

  it("panelHeader_longContextKeepsTitleFirstAndEllipsisHook", async () => {
    await mount(<PanelHeader title="代理" context={"漂移提醒".repeat(40)} actions={<Button>重载</Button>} />);
    const header = container.querySelector("header")!;
    const kids = [...header.children];
    expect(kids[0]!.textContent).toBe("代理");
    const context = kids.find((el) => el.getAttribute("title")?.startsWith("漂移提醒"));
    expect(context, "context 超长时应带 title 兜底（2 节 ellipsis＋tooltip）").toBeDefined();
    expect(kids[kids.length - 1]!.tagName).toBe("BUTTON");
  });
});
