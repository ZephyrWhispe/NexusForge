import { useState } from "react";
import { reportError } from "../stores/notifications";
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  makeStyles,
  tokens,
} from "@fluentui/react-components";

/**
 * T-B7-15 通用「干跑预览 → 清单确认 → 执行」对话框（共性③首例）。
 * 与业务解耦：调用方把待执行事实翻成 items 清单；本组件只负责
 * 展示清单 + 确认闸门（onConfirm 仅在点击「确认执行」后调用，
 * 执行中按钮忙态防双触发）。diff 预览 / 镜像同步等后续场景直接复用。
 */

export interface DryRunItem {
  /** 人读预览（一行一条） */
  text: string;
  /** true = 真执行风险项（徽标警示）；false = 仅展示项 */
  risky: boolean;
}

const useStyles = makeStyles({
  list: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    maxHeight: "320px",
    overflowY: "auto",
  },
  row: {
    display: "flex",
    alignItems: "baseline",
    gap: "8px",
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  text: {
    flex: 1,
    fontSize: tokens.fontSizeBase200,
    wordBreak: "break-all",
  },
  actions: { display: "flex", gap: "8px", justifyContent: "flex-end" },
  // 破坏性主按钮 = 红色（当前安装版 Fluent Button 无 overflow appearance）
  danger: {
    backgroundColor: tokens.colorPaletteRedBackground1,
    color: tokens.colorPaletteRedForeground1,
  },
});

export default function DryRunDialog(props: {
  open: boolean;
  title: string;
  items: DryRunItem[];
  /** 确认闸门：仅在点击「确认执行」后被调用（干跑阶段零端口触达的正证面） */
  onConfirm: () => Promise<void>;
  onCancel: () => void;
  confirmLabel?: string;
}) {
  const styles = useStyles();
  const [busy, setBusy] = useState(false);
  const riskyCount = props.items.filter((i) => i.risky).length;
  return (
    <Dialog
      open={props.open}
      onOpenChange={() => {
        if (!busy) props.onCancel();
      }}
    >
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{props.title}</DialogTitle>
          <DialogContent>
            <div className={styles.list} role="list">
              {props.items.map((item, i) => (
                <span key={i} role="listitem" className={styles.row}>
                  <span className={styles.text}>{item.text}</span>
                  <Badge
                    appearance="outline"
                    color={item.risky ? "danger" : "informative"}
                  >
                    {item.risky ? "真执行风险" : "仅展示"}
                  </Badge>
                </span>
              ))}
            </div>
          </DialogContent>
          <DialogActions className={styles.actions}>
            <Button appearance="subtle" disabled={busy} onClick={props.onCancel}>
              取消
            </Button>
            <Button
              appearance="primary"
              className={riskyCount > 0 ? styles.danger : undefined}
              disabled={busy}
              onClick={() => {
                setBusy(true);
                // STD-01：不再吞错——失败经全局错误通道可见（busy 收尾不变）
                void props
                  .onConfirm()
                  .catch((e) => reportError(e, { context: "确认执行失败" }))
                  .finally(() => setBusy(false));
              }}
            >
              {props.confirmLabel ?? "确认执行"}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
