import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import SubNav from "../SubNav";
import { SUBNAV, subnavActive, subnavSelect, type SubnavSelections, type SubNavScope } from "../modules";

// D-29 B3/T-B3-1 回归（09 §8.1-⑩ → §8.2）：二级导航从"一模块一语义"升为 scope 双维度。
// 承重断言是**两维度同屏且互不覆写**：只切视图时筛选高亮纹丝不动（反之亦然）——
// 这正是 MainWorkbench 按模块硬分叉时代的互污 bug（代理页点「分流」悄悄改掉剪切板筛选）的解毒剂。

const ST: SubnavSelections = {
  clipView: "stack",
  clipGroup: "all",
  proxySub: "nodes",
  syncSub: "conflicts",
};

let container: HTMLDivElement;
let root: Root;

function buttonByLabel(label: string): HTMLButtonElement {
  const found = [...document.querySelectorAll("button")].find(
    (b) => b.textContent?.trim().replace(/\d+$/, "") === label,
  );
  if (!found) throw new Error(`未找到导航项：${label}`);
  return found as HTMLButtonElement;
}

/** 高亮与否即 className 差异（makeStyles 的 filterOn 追加一枚类名），故同类状态字符串全等 */
function cls(label: string): string {
  return buttonByLabel(label).className;
}

async function renderActive(active: { view?: string; filter?: string }) {
  await act(async () => {
    root.render(<SubNav moduleId="clipboard" active={active} onSelect={() => {}} counts={{}} />);
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
});

afterEach(() => {
  act(() => root?.unmount());
  container.remove();
});

describe("SubNav 双维度（T-B3-1）", () => {
  it("subnav_clipboardViewsAndFilters_twoScopes：五视图 + 六筛选同屏，两维度高亮互不污染", async () => {
    await act(async () => {
      root = createRoot(container);
      root.render(
        <SubNav
          moduleId="clipboard"
          active={{ view: "stack", filter: "all" }}
          onSelect={() => {}}
          counts={{ all: 12, code: 3 }}
        />,
      );
    });

    // 两 section 同栏并存：视图五项 + 筛选六项逐项渲染（badgeKey 计数照常挂角标）
    expect(container.textContent).toContain("视图");
    expect(container.textContent).toContain("筛选");
    const viewItems = SUBNAV.clipboard.flatMap((s) => s.items).filter((i) => i.scope === "view");
    expect(viewItems.map((i) => i.label)).toEqual([
      "历史",
      "收藏与分组",
      "粘贴堆栈",
      "敏感库",
      "统计与设置",
    ]);
    for (const item of SUBNAV.clipboard.flatMap((s) => s.items))
      expect(buttonByLabel(item.label), `缺导航项 ${item.label}`).toBeInstanceOf(HTMLButtonElement);
    expect(buttonByLabel("全部").textContent).toBe("全部12");
    expect(buttonByLabel("代码").textContent).toBe("代码3");

    const offView = cls("历史");
    const onView = cls("粘贴堆栈");
    const onFilter = cls("全部");
    const offFilter = cls("文本");
    expect(onView).not.toBe(offView);
    expect(cls("收藏与分组")).toBe(offView);
    expect(onFilter).not.toBe(offFilter);

    // 只动视图维度 → 筛选维度高亮原样留着（分键，不互污）
    await renderActive({ view: "history", filter: "all" });
    expect(cls("全部")).toBe(onFilter);
    expect(cls("文本")).toBe(offFilter);
    expect(cls("历史")).toBe(onView);
    expect(cls("粘贴堆栈")).toBe(offView);

    // 只动筛选维度 → 视图维度高亮原样留着
    await renderActive({ view: "history", filter: "code" });
    expect(cls("历史")).toBe(onView);
    expect(cls("代码")).toBe(onFilter);
    expect(cls("全部")).toBe(offFilter);
  });

  it("subnav_clickCarriesScopeAndRouterMapsToOwnKey：点击回传维度、路由表落各自 session 键", async () => {
    const clicks: [SubNavScope, string][] = [];
    await act(async () => {
      root = createRoot(container);
      root.render(
        <SubNav
          moduleId="clipboard"
          active={{ view: "stack", filter: "all" }}
          onSelect={(scope, id) => clicks.push([scope, id])}
          counts={{}}
        />,
      );
    });
    for (const [label, want] of [
      ["敏感库", ["view", "secret"]],
      ["文本", ["filter", "text"]],
    ] as const) {
      await act(async () => {
        buttonByLabel(label).dispatchEvent(new MouseEvent("click", { bubbles: true }));
      });
      expect(clicks[clicks.length - 1]).toEqual(want);
    }

    expect(subnavActive("clipboard", "view", ST)).toBe("stack");
    expect(subnavActive("clipboard", "filter", ST)).toBe("all");
    expect(subnavActive("proxy", "filter", ST)).toBe("nodes");
    // 代理只有一维：view 维度无落键 → undefined（不得把剪切板的视图态错投给代理）
    expect(subnavActive("proxy", "view", ST)).toBeUndefined();
    expect(subnavActive("vault", "filter", ST)).toBeUndefined();

    expect(subnavSelect("clipboard", "view", "groups")).toEqual({ key: "clipView", value: "groups" });
    expect(subnavSelect("clipboard", "filter", "code")).toEqual({ key: "clipGroup", value: "code" });
    expect(subnavSelect("proxy", "filter", "rules")).toEqual({
      key: "proxySubPanel",
      value: "rules",
    });
    // T-B5-8：同步五档走第三枚分键 syncSubPanel（与代理/剪切板互不覆写）
    expect(subnavActive("sync", "filter", ST)).toBe("conflicts");
    expect(subnavActive("sync", "view", ST)).toBeUndefined();
    expect(subnavSelect("sync", "filter", "datasets")).toEqual({
      key: "syncSubPanel",
      value: "datasets",
    });
    // 跨维度串门与未注册 id 一律拒（野值不写 store）
    expect(subnavSelect("clipboard", "view", "all")).toBeUndefined();
    expect(subnavSelect("clipboard", "filter", "history")).toBeUndefined();
    expect(subnavSelect("clipboard", "filter", "nope")).toBeUndefined();
    expect(subnavSelect("vault", "filter", "all")).toBeUndefined();
    expect(subnavSelect("sync", "filter", "nope")).toBeUndefined();
    expect(subnavSelect("sync", "view", "overview")).toBeUndefined();
  });

  it("subnav_syncFiveItems_highlightIsolated", async () => {
    // SUBNAV.sync 五项在既有协议下逐项渲染（label 与 session id 一一对应）
    await act(async () => {
      root = createRoot(container);
      root.render(
        <SubNav moduleId="sync" active={{ filter: "devices" }} onSelect={() => {}} counts={{}} />,
      );
    });
    const items = SUBNAV.sync.flatMap((s) => s.items);
    expect(items.map((i) => i.label)).toEqual(["概览", "设备", "数据集", "冲突", "活动"]);
    for (const item of items)
      expect(buttonByLabel(item.label), `缺导航项 ${item.label}`).toBeInstanceOf(HTMLButtonElement);
    expect(cls("设备")).not.toBe(cls("概览"));

    // 切到 datasets：设备行退回未选中态，五档里恰一枚高亮（同维度内选择态不叠加、不外溢）
    const offOverview = cls("概览");
    const offActivity = cls("活动");
    await act(async () => {
      root.render(
        <SubNav moduleId="sync" active={{ filter: "datasets" }} onSelect={() => {}} counts={{}} />,
      );
    });
    const highlighted = items.filter((i) => cls(i.label) !== offOverview);
    expect(highlighted.map((i) => i.label)).toEqual(["数据集"]);
    expect(cls("活动")).toBe(offActivity);
  });
});
