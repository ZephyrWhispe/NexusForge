import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { ReactElement } from "react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";

import screenshotSrc from "../../modules/screenshot/ScreenshotPanel.tsx?raw";
import ocrSrc from "../../modules/ocr/OcrPanel.tsx?raw";
import netdiskSrc from "../../modules/file/NetdiskSection.tsx?raw";
import DeferredBadge, { deferredTooltipContent } from "../DeferredBadge";

// D-29 B0/T-B0-5 回归：延后范围必须以"看得见但不可点"的形式登记，
// Tooltip 携带 DECISIONS/批次出处；首批四个挂点（录屏/Paddle/每显示器/网盘）渲染可见。

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 已卸载 */
    }
  });
  container.remove();
});

async function mount(el: ReactElement) {
  await act(async () => {
    root = createRoot(container);
    // 应用根部即有 FluentProvider：复刻其 interaction store 上下文
    root.render(<FluentProvider theme={webLightTheme}>{el}</FluentProvider>);
  });
}

describe("DeferredBadge（T-B0-5）", () => {
  it("deferredBadge_tooltipCarriesDecisionRef：标签可见且同源 title 兜底携带出处", async () => {
    await mount(<DeferredBadge label="录屏" decisionRef="D-08" />);
    expect(container.textContent).toContain("录屏 · 延后");
    // 纯函数即 Tooltip 的数据源：出处与两份文档指路必须齐
    const tip = deferredTooltipContent("录屏", "D-08");
    expect(tip).toContain("D-08");
    expect(tip).toContain("docs/DECISIONS.md");
    expect(tip).toContain("docs/impl/09-blueprint-alignment.md");
    // 判据（确定性）：Badge 原生 title 兜底通道与 Tooltip content 同源，携带出处。
    // v9 Tooltip 浮层展开走 onPointerEnter/onFocus + keyborg/interaction 链
    //（probe 实测 jsdom 下无原生 pointer 事件可全真复现），浮层肉眼可见性归
    // 批次尾实启冒烟一并验收；此处钉死"出处字符串确实渲染进了 DOM"。
    const badge = container.querySelector("[aria-disabled='true']");
    expect(badge).not.toBeNull();
    expect(badge!.getAttribute("title")).toBe(deferredTooltipContent("录屏", "D-08"));
    expect(badge!.getAttribute("title")).toContain("依据 D-08");
  });

  it("deferredBadge_neverRendersAsEnabledButton：只可能是非可点的禁用徽标，不得是按钮", async () => {
    await mount(<DeferredBadge label="网盘" decisionRef="B6" />);
    expect(container.querySelector("button")).toBeNull();
    const el = container.querySelector("[aria-disabled='true']");
    expect(el).not.toBeNull();
    // Fluent v9 Badge 本体渲染为 div（jsdom 实测）：判据取"非可点元素"本质
    expect(["BUTTON", "A"]).not.toContain(el!.tagName);
    expect(el!.getAttribute("role")).not.toBe("button");
    expect(el!.hasAttribute("href")).toBe(false);
    // 首批挂点渲染可见（静态源断言：三面板各就其位）
    expect(String(screenshotSrc)).toContain('label="录屏" decisionRef="D-08"');
    expect(String(screenshotSrc)).toContain('label="每显示器覆盖层" decisionRef="D-23"');
    expect(String(ocrSrc)).toContain('label="PaddleOCR 引擎" decisionRef="D-08"');
    // T-B6-13 随行：file 域"网盘"挂点出处从批次号 `B6` 改钉 §6.3 档号（批次号会
    // 随批次完工过期成死引用，B5 同纪律）；本测试的"必须带出处"本质一条未动。
    // T-B7-27 随行换锚：网盘徽标随七档全拆从 FilePanel 挪进 NetdiskSection，
    // 徽标字面逐字未改（判据随事实走，B6 补记③纪律）。
    expect(String(netdiskSrc)).toContain('label="网盘" decisionRef="09 §6.3-(c)"');
  });
});
