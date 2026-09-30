import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";

import ListFooter from "../ListFooter";

// D-43 C9：页脚是可走查件却无钩子——真机对拍按 [data-nf] 取件，griffel 哈希类不可依赖（在册教训）。
// 像素高 32 属真窗量测项，jsdom 无布局引擎，此处只钉 DOM 形制与钩子在场。

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

describe("ListFooter（D-43 C9）", () => {
  it("listFooter_cdpHookAndSplitOrder_present", async () => {
    await mount(<ListFooter left="共 128 条" right="仅前 200 条" />);
    const footer = container.querySelector('[data-nf="list-footer"]');
    expect(footer, "真机走查依赖 data-nf 钩子命中页脚").not.toBeNull();
    const texts = [...footer!.children].map((el) => el.textContent?.trim());
    expect(texts[0], "左区先于右区").toBe("共 128 条");
    expect(texts[texts.length - 1], "右区恒为末位子节点").toBe("仅前 200 条");
  });
});
