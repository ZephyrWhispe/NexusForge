import { useState } from "react";
import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  makeStyles,
  Select,
  Text,
  tokens,
} from "@fluentui/react-components";
import type { KvmClientDevice, SendOutcome } from "./dragDropFlow";

/**
 * T-B8-1（D-33）拖拽传文件确认对话框——「预览→清单确认→执行」共性③在 kvm 域的形状：
 * 与 DryRunDialog 的差异是带目标设备选择槽（该组件无槽，为它扩会波及 automation
 * 既有消费面，裁决为自持清单+Select，登记见 DECISIONS D-33 决策①）。
 * **按次挂载**（父级 dropFiles 非空才渲染，B6 ConnectDialog 常驻定格教训）：
 * 设备选择初值取首台出站设备是安全的，因为每次 drop 都是全新挂载。
 */

const useStyles = makeStyles({
  pick: { display: "flex", alignItems: "center", gap: "8px", marginBottom: "10px" },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    maxHeight: "280px",
    overflowY: "auto",
  },
  item: {
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
    fontSize: tokens.fontSizeBase200,
    wordBreak: "break-all",
    fontFamily: "Consolas, monospace",
  },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  actions: { display: "flex", gap: "8px", justifyContent: "flex-end" },
});

export default function DragDropSendDialog(props: {
  files: readonly string[];
  devices: readonly KvmClientDevice[];
  /** 单文件发送腿：失败即抛（错误原文进聚合行） */
  sendOne: (deviceId: string, path: string) => Promise<void>;
  onDone: (outcomes: SendOutcome[]) => void;
  onClose: () => void;
}) {
  const styles = useStyles();
  const [deviceId, setDeviceId] = useState(props.devices[0]?.deviceId ?? "");
  const [busy, setBusy] = useState(false);

  const send = async () => {
    setBusy(true);
    const outcomes: SendOutcome[] = [];
    for (const path of props.files) {
      try {
        await props.sendOne(deviceId, path);
        outcomes.push({ path });
      } catch (e) {
        outcomes.push({ path, error: e instanceof Error ? e.message : String(e) });
      }
    }
    setBusy(false);
    props.onDone(outcomes);
  };

  return (
    <Dialog
      open
      onOpenChange={(_, d) => {
        if (!d.open && !busy) props.onClose();
      }}
    >
      <DialogSurface>
        <DialogBody>
          <DialogTitle>拖拽发送文件</DialogTitle>
          <DialogContent>
            <div className={styles.pick}>
              <Text size={200}>目标设备（仅出站会话）：</Text>
              <Select
                size="small"
                value={deviceId}
                disabled={busy}
                onChange={(_, d2) => setDeviceId(d2.value)}
              >
                {props.devices.map((d) => (
                  <option key={d.deviceId} value={d.deviceId}>
                    {d.deviceName}
                  </option>
                ))}
              </Select>
            </div>
            <div className={styles.list} role="list">
              {props.files.map((f) => (
                <span key={f} role="listitem" className={styles.item}>
                  {f}
                </span>
              ))}
            </div>
            <Text size={200} className={styles.muted}>
              共 {props.files.length} 个文件，逐个交既有发送通道；目录与非法路径由后端如实点名，不展开不猜测。
            </Text>
          </DialogContent>
          <DialogActions className={styles.actions}>
            <Button appearance="subtle" disabled={busy} onClick={props.onClose}>
              取消
            </Button>
            <Button
              appearance="primary"
              disabled={busy || deviceId === ""}
              onClick={() => void send()}
            >
              {busy ? "发送中…" : "发送"}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
