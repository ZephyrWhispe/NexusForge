import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";

import Section from "../../components/Section";
import SubNav from "../SubNav";
import { MODULES, SUBNAV, subnavActive, subnavSelect, type SubnavSelections } from "../modules";
import sectionSrc from "../../components/Section.tsx?raw";
import mainWorkbenchSrc from "../../windows/MainWorkbench.tsx?raw";

// D-43 C3：二级导航第三形制 anchor（同屏滚动锚点）的判据面。
// 锚点的本质是"选择不进 session"——SUBNAV_KEYS 不为 anchor 建键，读写两头必须同时静默，
// 否则点一次锚点就把 clipView/proxySubPanel 之类写成野值。
// 落点像素（scrollMarginTop=44 与 rect.top 区间）jsdom 无从证明，属实窗 CDP 走查项（C9）。

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

const selections: SubnavSelections = {
  clipView: "history",
  clipGroup: "all",
  proxySub: "overview",
  syncSub: "overview",
  fileSub: "browse",
};

/** 区块根以 data-nf="sec" 自证（同壳层 data-nf="work" 惯例，实窗 CDP 也按它枚举区块） */
function secRoots(): HTMLElement[] {
  return [...container.querySelectorAll('[data-nf="sec"]')] as HTMLElement[];
}

describe("锚点式二级导航（D-43 C3）", () => {
  it("section_anchorMarkerAppearsOnlyWhenAnchored", async () => {
    await mount(
      <>
        <Section title="无锚区块">内容</Section>
        <Section title="有锚区块" anchor="vault.health">
          内容
        </Section>
      </>,
    );
    const roots = secRoots();
    expect(roots.length, "两枚区块应各有一枚根").toBe(2);
    const [plain, anchored] = roots;
    expect(plain.getAttribute("data-nf-sec"), "未给 anchor 时不得输出空标记").toBeNull();
    expect(anchored.getAttribute("data-nf-sec")).toBe("vault.health");
    // 锚点态＝在基类之上追加 anchored 类（jsdom 无布局引擎，样式值归 CDP，这里只证两态确有两类）
    const extra = [...anchored.classList].filter((c) => !plain.classList.contains(c));
    expect(extra.length, "锚点态缺少独立类").toBeGreaterThan(0);
    expect([...plain.classList].filter((c) => !anchored.classList.contains(c)), "锚点态丢了区块基类").toEqual([]);
  });

  it("section_scrollMarginUsesSpecRowHeight_notAHandRoll", () => {
    // 锚点落点留白取 ROW_H.compact（44＝壳层工具条行高，§10-2 四的倍数档），禁手写 "44px"
    expect(sectionSrc).toContain("scrollMarginTop: ROW_H.compact");
    expect(sectionSrc).not.toMatch(/scrollMarginTop:\s*"\d+px"/);
    expect(sectionSrc).toContain("data-nf-sec={anchor}");
  });

  it("anchorScopeNeverWritesNorReadsSession_forEveryModule", () => {
    for (const m of MODULES) {
      expect(subnavSelect(m.id, "anchor", "anything"), `${m.id} 的 anchor 选择不得派发 session 键`).toBeUndefined();
      expect(subnavActive(m.id, "anchor", selections), `${m.id} 的 anchor 不得读任何 session 键`).toBeUndefined();
    }
    // 已有两维（clipboard 的 view/filter）在 anchor 静默的同时不受牵连
    expect(subnavSelect("clipboard", "view", "stack")).toEqual({ key: "clipView", value: "stack" });
  });

  it("subNav_anchorItemsHighlightLocallyAndReportAnchorScope", async () => {
    // 用注册表真值（C4 起 vault 两枚锚点在册，不再临时注入——注入档会让"导航指向不存在区块"
    // 这类真缺陷在本档 invisible）
    const items = SUBNAV.vault.flatMap((s) => s.items);
    expect(items.map((i) => i.scope), "vault 导航应全为 anchor 形制").toEqual([
      "anchor",
      "anchor",
    ]);
    const onSelect = vi.fn();
    await mount(
      <SubNav moduleId="vault" active={{ view: undefined, filter: undefined }} onSelect={onSelect} />,
    );
    const buttons = [...container.querySelectorAll("button")];
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(
      items.map((i) => i.label),
    );
    expect(buttons[0]!.getAttribute("aria-current"), "首帧不得有高亮锚点（无持久化选择态）").toBeNull();

    await act(async () => {
      buttons[1]!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onSelect).toHaveBeenCalledWith("anchor", items[1]!.id);
    expect(buttons[1]!.getAttribute("aria-current")).toBe("true");
    expect(buttons[0]!.getAttribute("aria-current"), "锚点高亮单选，不得两枚同亮").toBeNull();
  });

  it("mainWorkbench_anchorBranchIsTheScrollOwnerAndKeysTheRailPerModule", () => {
    // 机理钉：三处缺一即失效——分支存在（否则 anchor 落进 session 派发）、
    // 查找根是内容区（否则跨模块串锚）、SubNav 按模块 remount（否则当前锚点漏进无关面板）
    expect(mainWorkbenchSrc).toContain('if (scope === "anchor")');
    expect(mainWorkbenchSrc).toContain("scrollIntoView({ block: \"start\", behavior: \"smooth\" })");
    expect(mainWorkbenchSrc).toContain("querySelectorAll(\"[data-nf-sec]\")");
    expect(mainWorkbenchSrc).toContain("<SubNav\n              key={moduleId}");
    expect(mainWorkbenchSrc).toContain('ref={contentRef}');
    // 锚点查找走属性比对而非拼选择器（注册表将来含特殊字符不会破）
    expect(mainWorkbenchSrc).toContain('el.getAttribute("data-nf-sec") === id');
  });
});
