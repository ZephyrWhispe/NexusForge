import { NF_MISSING } from "../../components/nfTiers";

/**
 * D-42：KVM 逐文件传输动态（事件面纯函数，KvmPanel 只做订阅与渲染）。
 *
 * 后端把逐文件进度只放在事件总线上（kvm-core/module.rs:295 起经
 * publish_merged 200ms 合并，key=transfer_id；`kvm.session_state` 的断开原因
 * 亦只在此），**不落库、无查询命令**——面板不接就等于这条能力从未存在。
 * 因此这些行是"实时视图"而不是事实源：不做持久声明、不谎称可回看，
 * 行数封顶只保最近若干条（D-42 风险登记⑤：后端合并窗口丢帧时这里显示滞后，
 * 而不是把滞后说成已完成）。
 */

export type TransferDirection = "in" | "out";
export type TransferState = "active" | "done" | "failed";

export interface TransferRow {
  transferId: string;
  name: string;
  deviceId: string;
  direction: TransferDirection;
  done: number;
  total: number;
  state: TransferState;
  note?: string;
}

/** 保留最近 N 条（事件流无终局清理命令，靠上限防无限增长） */
export const MAX_TRANSFER_ROWS = 8;

/** 发送腿在拿到 transfer_id 之前就失败时后端回 transfer_id:null（module.rs:315） */
export const ACK_NO_TRANSFER_ID = "（未建立传输）";

const num = (v: unknown): number => (typeof v === "number" && Number.isFinite(v) ? v : 0);
const str = (v: unknown): string => (typeof v === "string" ? v : "");

function upsert(
  rows: TransferRow[],
  transferId: string,
  apply: (row: TransferRow) => TransferRow,
): TransferRow[] {
  const i = rows.findIndex((r) => r.transferId === transferId);
  if (i >= 0) {
    const next = [...rows];
    next[i] = apply(next[i]);
    return next.slice(-MAX_TRANSFER_ROWS);
  }
  const base: TransferRow = {
    transferId,
    name: "",
    deviceId: "",
    direction: "out",
    done: 0,
    total: 0,
    state: "active",
  };
  return [...rows, apply(base)].slice(-MAX_TRANSFER_ROWS);
}

/**
 * 把一枚 kvm.* 事件折进传输行集合。返回原数组引用＝本事件与传输动态无关
 * （订阅侧据此判断"这只不是门铃该不该另作处理"，也免掉无谓的重渲染）。
 */
export function applyKvmTransferEvent(
  rows: TransferRow[],
  topic: string,
  p: Record<string, unknown>,
): TransferRow[] {
  switch (topic) {
    case "kvm.file_progress": {
      const transferId = str(p.transfer_id);
      if (!transferId) return rows;
      const sent = num(p.sent_chunks);
      const total = num(p.total_chunks);
      return upsert(rows, transferId, (r) => ({
        ...r,
        direction: "out",
        done: Math.max(r.done, sent),
        total: total || r.total,
      }));
    }
    case "kvm.file_incoming": {
      const transferId = str(p.transfer_id);
      if (!transferId) return rows;
      const received = num(p.received);
      return upsert(rows, transferId, (r) => ({
        ...r,
        direction: "in",
        deviceId: str(p.device_id) || r.deviceId,
        name: str(p.name) || r.name,
        done: Math.max(r.done, received),
        total: num(p.total_chunks) || r.total,
      }));
    }
    case "kvm.file_received": {
      const transferId = str(p.transfer_id);
      if (!transferId) return rows;
      return upsert(rows, transferId, (r) => ({
        ...r,
        direction: "in",
        deviceId: str(p.device_id) || r.deviceId,
        name: str(p.name) || r.name,
        state: "done",
        done: r.total || r.done,
        note: str(p.path) || r.note,
      }));
    }
    case "kvm.transfer_ack": {
      const transferId = str(p.transfer_id) || ACK_NO_TRANSFER_ID;
      const ok = p.ok === true;
      return upsert(rows, transferId, (r) => ({
        ...r,
        name: r.name || (transferId === ACK_NO_TRANSFER_ID ? "文件发送失败" : ""),
        state: ok ? "done" : "failed",
        done: ok ? r.total || r.done : r.done,
        note: ok ? r.note : str(p.error) || "对端未给出原因",
      }));
    }
    default:
      return rows;
  }
}

/** 文件列文本：发送腿的事件不带名称（后端只有 transfer_id），用短码占位而不编造 */
export function transferLabel(row: TransferRow): string {
  if (row.name) return row.name;
  return row.transferId === ACK_NO_TRANSFER_ID
    ? "文件发送失败"
    : `传输 ${row.transferId.slice(0, 8)}`;
}

/** 进度列文本：总块数未知时显缺值占位，不用 0/0 冒充"刚要开始" */
export function transferProgressText(row: TransferRow): string {
  if (row.total <= 0) return NF_MISSING;
  return `${Math.min(row.done, row.total)}/${row.total}`;
}

export function transferStateLabel(row: TransferRow): string {
  if (row.state === "done") return "完成";
  if (row.state === "failed") return "失败";
  return "进行中";
}

export const transferDirectionLabel: Record<TransferDirection, string> = {
  in: "接收",
  out: "发送",
};
