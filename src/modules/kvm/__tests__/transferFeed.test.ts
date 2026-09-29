/**
 * D-42：KVM 逐文件传输动态的事件面纯函数回归。
 * 后端四枚主题的载荷形制（kvm-core/module.rs:297/675/685/758 与 :315 的 null-id 腿）
 * 是这里唯一的判据来源——凭想象写断言等于把契约钉错。
 */
import { describe, expect, it } from "vitest";

import {
  ACK_NO_TRANSFER_ID,
  MAX_TRANSFER_ROWS,
  applyKvmTransferEvent,
  transferLabel,
  transferProgressText,
  transferStateLabel,
  type TransferRow,
} from "../transferFeed";

const row = (p: Partial<TransferRow>): TransferRow => ({
  transferId: "t1",
  name: "",
  deviceId: "",
  direction: "out",
  done: 0,
  total: 0,
  state: "active",
  ...p,
});

const progress = (rows: TransferRow[], p: Record<string, unknown>) =>
  applyKvmTransferEvent(rows, "kvm.file_progress", p);

describe("applyKvmTransferEvent（D-42 事件面）", () => {
  it("fileProgress_foldsIntoOneRowAndNeverRegresses", () => {
    let rows = progress([], { transfer_id: "a", sent_chunks: 3, total_chunks: 10 });
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ transferId: "a", direction: "out", done: 3, total: 10 });
    rows = progress(rows, { transfer_id: "a", sent_chunks: 7, total_chunks: 10 });
    expect(rows).toHaveLength(1);
    expect(rows[0].done).toBe(7);
    // 合并窗口内迟到一帧旧快照不得把进度倒退（后端 publish_merged 只保末值，
    // 但订阅端多路转发没有这个保证，本地兜住）
    rows = progress(rows, { transfer_id: "a", sent_chunks: 5, total_chunks: 10 });
    expect(rows[0].done).toBe(7);
  });

  it("emptyTransferId_isDroppedNotRendered", () => {
    // 进度腿缺 id 无从归并 ⇒ 原数组同一引用返回（订阅侧据此免重渲染）
    const rows: TransferRow[] = [];
    expect(progress(rows, { sent_chunks: 1, total_chunks: 2 })).toBe(rows);
    expect(applyKvmTransferEvent(rows, "kvm.file_incoming", { name: "x" })).toBe(rows);
    expect(applyKvmTransferEvent(rows, "kvm.file_received", { name: "x" })).toBe(rows);
  });

  it("ackFailureWithoutTransferId_staysVisible", () => {
    // module.rs:315：transfer::send_file 自身失败时 transfer_id 恒为 null，
    // 这条腿若按"空 id 即丢"处理，发送失败就全静默了
    const rows = applyKvmTransferEvent([], "kvm.transfer_ack", {
      transfer_id: null,
      ok: false,
      error: "设备 dev 无活跃会话",
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].transferId).toBe(ACK_NO_TRANSFER_ID);
    expect(transferLabel(rows[0])).toBe("文件发送失败");
    expect(transferStateLabel(rows[0])).toBe("失败");
    expect(rows[0].note).toBe("设备 dev 无活跃会话");
  });

  it("fileReceived_marksDoneAndClampsProgress", () => {
    let rows = progress([], { transfer_id: "b", sent_chunks: 4, total_chunks: 4 });
    rows = applyKvmTransferEvent(rows, "kvm.file_received", {
      device_id: "d",
      transfer_id: "b",
      name: "报告.pdf",
      path: "C:/tmp/报告.pdf",
    });
    expect(rows[0]).toMatchObject({
      state: "done",
      name: "报告.pdf",
      deviceId: "d",
      note: "C:/tmp/报告.pdf",
    });
    expect(transferProgressText(rows[0])).toBe("4/4");
    expect(transferStateLabel(rows[0])).toBe("完成");
  });

  it("incomingRow_keepsUnknownTotalAsMissingPlaceholder", () => {
    // 只收到 progress 之前的行（面板晚打开）：总块数未知不得显示成 0/0
    const rows = applyKvmTransferEvent([], "kvm.file_incoming", {
      device_id: "d9",
      transfer_id: "z",
      name: "movie.mkv",
      size: 123,
      received: 2,
    });
    expect(transferProgressText(rows[0])).toBe("--");
    expect(rows[0]).toMatchObject({ direction: "in", name: "movie.mkv", done: 2 });
  });

  it("unrelatedTopic_returnsSameReference", () => {
    const rows = [row({})];
    expect(applyKvmTransferEvent(rows, "kvm.peer_online", { peer: {} })).toBe(rows);
    expect(applyKvmTransferEvent(rows, "clipboard.captured", { id: 1 })).toBe(rows);
  });

  it("rowCap_keepsMostRecentEntriesBounded", () => {
    let rows: TransferRow[] = [];
    for (let i = 0; i < MAX_TRANSFER_ROWS + 5; i += 1) {
      rows = progress(rows, { transfer_id: `t${i}`, sent_chunks: 1, total_chunks: 2 });
    }
    expect(rows).toHaveLength(MAX_TRANSFER_ROWS);
    expect(rows[rows.length - 1].transferId).toBe(`t${MAX_TRANSFER_ROWS + 4}`);
    expect(rows.some((r) => r.transferId === "t0")).toBe(false);
  });
});
