import { describe, expect, it } from "vitest";

import kvmPanelSrc from "../KvmPanel.tsx?raw";

// D-29 B7/T-B7-7 前端零扰动正对照：XButton/水平滚轮投影全程走 RawInput 线格式 +
// win-integration capture/inject 两端，KvmPanel 输入面（配对码/边缘/剪贴板推送）本批
// 零改动。负钉=投影实现词不得进面板（前端不拆 button/delta 字段，出现即形制漂移）；
// 正钉=?raw 真读到了源且既有输入锚在册（防空读/拼错假绿）。

describe("KvmPanel 输入矩阵零扰动（T-B7-7 正对照）", () => {
  it("kvmPanel_inputMatrixUnchanged：投影词禁入面板，既有输入锚逐字在册", () => {
    const src = String(kvmPanelSrc);
    // 负钉：线格式字段名/投影中文词不得出现在面板源里
    for (const banned of ["XButton", "xbutton", "horizontal", "侧键", "水平滚轮", "HWHEEL"]) {
      expect(src, `KvmPanel 不得出现投影面词 ${banned}`).not.toContain(banned);
    }
    // 正对照（防"零命中是因为什么都没读"）：源体量 + 既有输入面锚逐字在册
    expect(src.length, "?raw 源装载失败").toBeGreaterThan(1000);
    expect(src).toContain("本端配对码");
    expect(src).toContain("配对流程");
    expect(src).toContain("推送本机剪贴板文本");
  });
});
