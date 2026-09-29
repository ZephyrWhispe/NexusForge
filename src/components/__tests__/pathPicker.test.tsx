import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";

import PathPicker from "../PathPicker";

// D-42 组件族回归（00-ui-layout-spec 3 节"禁逼用户手打绝对路径"＋4-3 inline 校验时序）。
// 判据：浏览钮恒在（手打不是唯一通路）、错误文案与输入框经 aria-describedby 相关
//（读屏用户听得到校验），禁用态两件套同步。

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
    // 同一用例内二次 mount 复用 root（React 警告：容器已 createRoot 过）
    if (!root) root = createRoot(container);
    root.render(<FluentProvider theme={webLightTheme}>{el}</FluentProvider>);
  });
}

function input(): HTMLInputElement {
  const el = container.querySelector("input");
  if (!el) throw new Error("PathPicker 未渲染输入框");
  return el;
}

function browseButton(): HTMLButtonElement {
  const el = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === "浏览",
  );
  if (!el) throw new Error("PathPicker 缺「浏览」按钮");
  return el as HTMLButtonElement;
}

describe("PathPicker（D-42）", () => {
  it("pathPicker_browseIsAlwaysPresentAndCallbackDriven：浏览钮恒在且只回调，不自持对话框", async () => {
    const onBrowse = vi.fn();
    await mount(<PathPicker value="" onChange={vi.fn()} onBrowse={onBrowse} placeholder="选择目录" />);
    expect(input().placeholder).toBe("选择目录");
    await act(async () => {
      browseButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onBrowse).toHaveBeenCalledTimes(1);
  });

  it("pathPicker_errorIsLinkedByAriaDescribedby：错误经 aria-describedby 关联到输入框（4-3）", async () => {
    await mount(<PathPicker value="D:\\缺目录" onChange={vi.fn()} onBrowse={vi.fn()} error="路径不存在" />);
    const described = input().getAttribute("aria-describedby");
    expect(described, "错误在场时输入框须带 aria-describedby").toBeTruthy();
    const node = [...container.querySelectorAll("[id]")].find((el) => el.id === described);
    expect(node, "被指向的 id 必须真在场").not.toBeNull();
    expect(node!.textContent).toBe("路径不存在");
    // 正对照：无错误时不伪造关联（空描述指向不存在节点＝读屏噪声）
    await mount(<PathPicker value="D:\\ok" onChange={vi.fn()} onBrowse={vi.fn()} />);
    expect(input().hasAttribute("aria-describedby")).toBe(false);
  });

  it("pathPicker_typingRoundTripsThroughOnChange：键入值原样回传（不夹带裁剪）", async () => {
    const onChange = vi.fn();
    await mount(<PathPicker value="a" onChange={onChange} onBrowse={vi.fn()} />);
    await act(async () => {
      const el = input();
      Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")!.set!.call(
        el,
        "C:\\Users\\nf\\笔记",
      );
      el.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(onChange).toHaveBeenCalledWith("C:\\Users\\nf\\笔记");
  });

  it("pathPicker_disabledTakesBothControlsOut：禁用态输入框与浏览钮同步退出", async () => {
    await mount(<PathPicker value="" onChange={vi.fn()} onBrowse={vi.fn()} disabled />);
    expect(input().disabled).toBe(true);
    expect(browseButton().disabled).toBe(true);
  });
});
