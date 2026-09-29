import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Button, FluentProvider, Switch, webLightTheme } from "@fluentui/react-components";

import SwitchSetting, { SettingsGroup } from "../SwitchSetting";

// D-42 组件族回归（00-ui-layout-spec 6 节末端对齐＋4-2 禁用必解释＋8 节可判定化三条）。
// "控件右缘对齐同一基线"在 DOM 上的等价形＝控件恒为行的最后一个元素（data-nf 钩子命中）；
// 像素级右缘实得归批尾真机骨架走查（jsdom 无布局引擎，不在此伪称量过宽度）。

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

function settingRows(): Element[] {
  return [...container.querySelectorAll('[data-nf="setting-row"]')];
}

function controlSlot(row: Element): Element {
  const slot = row.lastElementChild!;
  expect(slot.getAttribute("data-nf"), "行末槽位须是控件容器").toBe("setting-control");
  return slot;
}

describe("SwitchSetting / SettingsGroup（D-42）", () => {
  it("switchSetting_controlIsLastElement_threeRowsShareTheTailSlot", async () => {
    await mount(
      <SettingsGroup title="截图">
        <SwitchSetting title="开机自启" description="随系统登录启动宿主" control={<Switch label="开机自启" />} />
        <SwitchSetting title="含隐私内容" control={<Switch label="含隐私内容" />} />
        <SwitchSetting
          title="上传目标"
          description="未配置图床"
          control={
            <Button appearance="subtle" onClick={vi.fn()}>
              配置
            </Button>
          }
        />
      </SettingsGroup>,
    );
    expect(container.textContent).toContain("截图");
    expect(container.textContent).toContain("随系统登录启动宿主");
    expect(settingRows()).toHaveLength(3);
    for (const row of settingRows()) {
      expect(
        controlSlot(row).querySelector("[role='switch'], button, input"),
        "行末槽位承载控件（末端对齐基线）",
      ).not.toBeNull();
    }
  });

  it("switchSetting_disabledHintBecomesTitle：禁用说明原话进控件容器 title（4-2 钩子）", async () => {
    await mount(
      <SwitchSetting
        title="远程上传"
        control={<Button disabled>上传</Button>}
        disabledHint="内核未运行，先到总览启动"
      />,
    );
    const slot = controlSlot(settingRows()[0]!);
    expect(slot.getAttribute("title")).toBe("内核未运行，先到总览启动");
  });

  it("switchSetting_withoutHintRendersNoEmptyTitle：负例——未给说明就不伪造空 title", async () => {
    await mount(<SwitchSetting title="纯开关" control={<Switch label="纯开关" />} />);
    expect([...container.querySelectorAll("[title='']")]).toEqual([]);
    expect(controlSlot(settingRows()[0]!).hasAttribute("title")).toBe(false);
  });

  it("settingsGroup_restoreDefaultsIsOptionalAndWired：右上角恢复默认钮存在并可点，缺省不渲染", async () => {
    const onRestoreDefaults = vi.fn();
    await mount(
      <SettingsGroup title="OCR" onRestoreDefaults={onRestoreDefaults}>
        <SwitchSetting title="引擎" control={<Switch label="引擎" />} />
      </SettingsGroup>,
    );
    const restore = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "恢复默认",
    );
    expect(restore).toBeDefined();
    await act(async () => {
      restore!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onRestoreDefaults).toHaveBeenCalledTimes(1);

    await mount(
      <SettingsGroup title="OCR">
        <SwitchSetting title="引擎" control={<Switch label="引擎" />} />
      </SettingsGroup>,
    );
    expect(
      [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === "恢复默认"),
      "未传回调即不出现恢复默认钮（无投机 UI）",
    ).toBeUndefined();
  });
});
