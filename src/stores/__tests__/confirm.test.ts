import { describe, expect, it } from "vitest";
import { confirmAction, impactLines, useConfirmStore } from "../confirm";

// D-18 二次确认通道：影响面归一化 + FIFO 队列语义（宿主/取消/Esc 复用 settleHead）
describe("impactLines", () => {
  it("缺省 → 空数组", () => {
    expect(impactLines({})).toEqual([]);
  });

  it("字符串 → 单行", () => {
    expect(impactLines({ impact: "将删除 12 条记录" })).toEqual(["将删除 12 条记录"]);
  });

  it("数组原样返回", () => {
    expect(impactLines({ impact: ["3 个文件", "约 1.2 GB"] })).toEqual(["3 个文件", "约 1.2 GB"]);
  });
});

describe("confirmAction 队列", () => {
  const queueOf = () => useConfirmStore.getState().queue.map((r) => r.title);

  it("并发请求按 FIFO 排队，settleHead 逐项兑现 resolve", async () => {
    const a = confirmAction({ title: "A", impact: "1 项" });
    const b = confirmAction({ title: "B" });
    expect(queueOf()).toEqual(["A", "B"]);

    useConfirmStore.getState().settleHead(true);
    expect(await a).toBe(true);
    expect(queueOf()).toEqual(["B"]);

    // Esc / 遮罩点击同样走 settleHead(false)
    useConfirmStore.getState().settleHead(false);
    expect(await b).toBe(false);
    expect(useConfirmStore.getState().queue).toHaveLength(0);
  });

  it("结算后 resolve 送达，空队列 settleHead 为无害 no-op", async () => {
    const pending = confirmAction({ title: "C" });
    let settled = false;
    void pending.then(() => {
      settled = true;
    });
    useConfirmStore.getState().settleHead(true);
    expect(await pending).toBe(true);
    expect(settled).toBe(true);
    useConfirmStore.getState().settleHead(true);
    expect(useConfirmStore.getState().queue).toHaveLength(0);
  });

  it("重复结算同一项不会二次 resolve", async () => {
    const only = confirmAction({ title: "D" });
    useConfirmStore.getState().settleHead(true);
    useConfirmStore.getState().settleHead(true); // 队列已空，no-op
    expect(await only).toBe(true);
  });
});
