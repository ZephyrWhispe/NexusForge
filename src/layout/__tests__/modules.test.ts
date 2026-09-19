import { describe, expect, it } from "vitest";

import { MODULES } from "../modules";

// D-08 必须同步动作：导航/标题不得暗示 v1.1 能力（录屏、PaddleOCR、翻译）可用。
// 名称表是唯一承诺面，此测试把红线钉死，防止后续文案回潮。
describe("MODULES v1 promise wording (D-08/D-09)", () => {
  const BANNED = ["录屏", "翻译", "Paddle", "录制"];

  it("no module name advertises v1.1-only capabilities", () => {
    for (const m of MODULES) {
      for (const word of BANNED) {
        expect(m.name, `${m.id} 名称不应承诺 ${word}`).not.toContain(word);
      }
    }
  });

  it("screenshot and ocr keep stable routing ids", () => {
    const ids = MODULES.map((m) => m.id);
    expect(ids).toContain("screenshot");
    expect(ids).toContain("ocr");
    expect(new Set(ids).size).toBe(ids.length);
    expect(MODULES).toHaveLength(14); // 全导航项钉死，增删须同步 DESIGN §3
  });
});
