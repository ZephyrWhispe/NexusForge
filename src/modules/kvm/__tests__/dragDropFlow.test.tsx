import { describe, expect, it } from "vitest";
import { aggregateSends, planDropSend, type KvmClientDevice } from "../dragDropFlow";

/** T-B8-1（D-33）拖拽传文件纯核四枚（11 档 §8 回归列字面名）。 */

const devB: KvmClientDevice = { deviceId: "dev-b", deviceName: "台式机B" };
const devC: KvmClientDevice = { deviceId: "dev-c", deviceName: "笔记本C" };

describe("kvm drag-drop planner (T-B8-1)", () => {
  it("kvmDragDrop_emptyPaths_refusesBeforeDialog", () => {
    for (const paths of [[], ["", "   "]]) {
      const plan = planDropSend(paths, [devB]);
      expect(plan.kind).toBe("refuse");
      if (plan.kind === "refuse") expect(plan.reason).toContain("未拖入任何文件");
    }
  });

  it("kvmDragDrop_noClientSessions_namedRefuse", () => {
    const plan = planDropSend(["C:/docs/a.pdf"], []);
    expect(plan.kind).toBe("refuse");
    if (plan.kind === "refuse") {
      // 拒因点名前置动作（"连接"）与会话语义，不开对话框
      expect(plan.reason).toContain("连接");
      expect(plan.reason).toContain("出站会话");
    }
  });

  it("kvmDragDrop_readyVerbatimFileOrder", () => {
    const files = ["D:/tmp/报告 v2.docx", "C:/a.txt", "C:/b.bin"];
    const plan = planDropSend(files, [devB, devC]);
    expect(plan.kind).toBe("ready");
    if (plan.kind === "ready") {
      expect(plan.files).toEqual(files);
      expect(plan.devices).toEqual([devB, devC]);
    }
  });

  it("kvmDragDrop_aggregate_countsPerLetterAndLines", () => {
    const agg = aggregateSends([
      { path: "C:/a.txt" },
      { path: "C:/b.bin", error: "设备 dev-b 无活跃会话" },
      { path: "C:/c.tmp", error: "打开文件失败: 拒绝访问" },
    ]);
    expect(agg.ok).toBe(1);
    expect(agg.failed).toBe(2);
    expect(agg.lines).toEqual([
      "失败 C:/b.bin：设备 dev-b 无活跃会话",
      "失败 C:/c.tmp：打开文件失败: 拒绝访问",
    ]);
    const none = aggregateSends([]);
    expect(none).toEqual({ ok: 0, failed: 0, lines: [] });
  });
});
