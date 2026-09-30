import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";

import CapabilityCard from "../CapabilityCard";
import { capabilitiesFor } from "../../layout/capabilities";
import { MODULES, type ModuleId } from "../../layout/modules";
import { PANELS } from "../../layout/panels";

// D-43 ③ 能力卡回归。夹具形状取本仓既有谱（createRoot＋act，无 testing-library）。
// 三条钉子＝"无引导"的可机检表达：首帧 aria-expanded=false、无 detail 的面板连展开钮都不给、
// 展开区四段字样齐且"尚不做"一律复用既有 DeferredBadge 徽标（不新建第二种延后语义）。

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

async function mount(id: ModuleId) {
  await act(async () => {
    if (!root) root = createRoot(container);
    root.render(
      <FluentProvider theme={webLightTheme}>
        <CapabilityCard capabilities={capabilitiesFor(id)} />
      </FluentProvider>,
    );
  });
}

const text = () => container.textContent ?? "";

function toggle(): HTMLElement {
  const btn = container.querySelector("button");
  if (!btn) throw new Error('能力卡未渲染展开钮（<button type="button">）');
  return btn;
}

describe("CapabilityCard（D-43 ③）", () => {
  it("capability_collapsedIsSingleSourceFromPanels：14 枚 collapsed 逐字等于 PANELS subtitle", () => {
    for (const m of MODULES) {
      const cap = capabilitiesFor(m.id);
      expect(cap.collapsed, `${m.id} 的 collapsed 漂移`).toBe(PANELS[m.id].subtitle);
      expect(cap.collapsed.trim().length, `${m.id} collapsed 为空`).toBeGreaterThan(0);
    }
  });

  it("capability_detailCoversExactlyTheFourReviewedPanels：detail 恰四枚，其余十枚不可展开", () => {
    const withDetail = MODULES.filter((m) => capabilitiesFor(m.id).detail !== undefined)
      .map((m) => m.id)
      .sort();
    expect(withDetail).toEqual(["notes", "sys", "term", "vault"]);
    for (const m of MODULES) {
      const d = capabilitiesFor(m.id).detail;
      if (!d) continue;
      for (const [dim, list] of Object.entries(d)) {
        expect(list.length, `${m.id}.${dim} 空清单`).toBeGreaterThan(0);
        for (const s of list) expect(s.trim().length, `${m.id}.${dim} 空条目`).toBeGreaterThan(0);
      }
    }
  });

  it("capability_firstFrameCollapsedNeverAutoExpands：首帧一行折叠且展开区不在场", async () => {
    await mount("vault");
    expect(toggle().getAttribute("aria-expanded"), "首帧即展开＝自动引导").toBe("false");
    expect(text()).toContain("Argon2id 信封 · AES-256-GCM 条目 · TOTP");
    expect(text(), "展开区字样首帧泄漏").not.toContain("尚不做");
    expect(text()).not.toContain("上限与截断");
  });

  it("capability_expandedShowsFourSectionsAndDeferredBadges：四段齐·延后徽标枚数=notYet·快捷键成 kbd", async () => {
    const d = capabilitiesFor("sys").detail!;
    await mount("sys");
    await act(async () => {
      toggle().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
    for (const head of ["能做", "尚不做", "上限与截断", "快捷键"]) {
      expect(text(), `缺展开段 ${head}`).toContain(head);
    }
    for (const s of [...d.can, ...d.limits]) expect(text(), `条目未上屏：${s}`).toContain(s);
    const badges = [...container.querySelectorAll("[aria-disabled='true']")].map((b) =>
      (b.textContent ?? "").trim(),
    );
    expect(badges, "延后徽标枚数/文案与 notYet 不等").toEqual(
      d.notYet.map((s) => `${s} · 延后`),
    );
    const keys = [...container.querySelectorAll("kbd")];
    expect(keys).toHaveLength(d.keys.length);
    expect(keys[0]?.textContent).toBe(d.keys[0]!.split("：")[0]);
  });

  it("capability_noDetailPanelHasNoToggle：未核读面板只渲一行，连展开钮都不给", async () => {
    await mount("screenshot");
    expect(container.querySelector("button"), "无 detail 却给出展开钮＝空清单装完成").toBeNull();
    expect(text()).toContain(PANELS.screenshot.subtitle);
  });
});
