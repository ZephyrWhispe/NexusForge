import { useState } from "react";
import { reportError } from "../../stores/notifications";
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
  Text,
  tokens,
} from "@fluentui/react-components";
import type { NameFixItemDto } from "../../ipc/client";

/**
 * T-B7-26 远端名 Ask 预览确认对话框（00-spec 共性③"预览→清单确认→执行"
 * 的 file 域名闸实例）。与调用解耦：预览行 = items 逐行复述 原名 → 建议名
 * 与冲突字符归因；onConfirm 仅在点击「按建议名继续」后恰调用一次——
 * 任何一行没有干净建议（suggested=null）时确认钮禁用，禁半截承诺。
 */

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
    flexDirection: "column",
    gap: "4px",
    padding: "6px 8px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  nameLine: {
    display: "flex",
    alignItems: "baseline",
    gap: "8px",
    fontSize: tokens.fontSizeBase300,
    wordBreak: "break-all",
  },
  arrow: { fontWeight: tokens.fontWeightBold },
  badLine: { display: "flex", flexWrap: "wrap", gap: "6px", alignItems: "center" },
  actions: { display: "flex", gap: "8px", justifyContent: "flex-end" },
});

export default function NameFixDialog(props: {
  open: boolean;
  items: NameFixItemDto[];
  /** 确认闸门：仅在点击「按建议名继续」后调用（调用方带 name_fix:"auto_rename" 重投） */
  onConfirm: () => Promise<void>;
  onCancel: () => void;
}) {
  const styles = useStyles();
  const [busy, setBusy] = useState(false);
  // 无事实源就无文案：任何一行映射不出干净新名，整批都不给确认——
  // AutoRename 臂在服务端对无建议行恒 Err，这里禁点即是同一裁决的前投
  const unfixable = props.items.some((i) => i.suggested === null);
  return (
    <Dialog
      open={props.open}
      onOpenChange={() => {
        if (!busy) props.onCancel();
      }}
    >
      <DialogSurface>
        <DialogBody>
          <DialogTitle>远端名字符冲突 · {props.items.length} 项待裁决</DialogTitle>
          <DialogContent>
            <div className={styles.list} role="list">
              {props.items.map((item) => (
                <span key={item.name} role="listitem" className={styles.row}>
                  <span className={styles.nameLine}>
                    <Text size={300}>{item.name}</Text>
                    <Text size={300} className={styles.arrow}>
                      →
                    </Text>
                    <Text size={300} weight="semibold">
                      {item.suggested ?? "（映射不出干净新名）"}
                    </Text>
                  </span>
                  <span className={styles.badLine}>
                    {item.bad.map((b) => (
                      <Badge key={b.char} appearance="outline" color="danger" title={b.reason}>
                        「{b.char}」{b.reason}
                      </Badge>
                    ))}
                  </span>
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
              disabled={busy || unfixable}
              title={unfixable ? "存在映射不出干净新名的行，逐行改名后再投" : undefined}
              onClick={() => {
                setBusy(true);
                // STD-01：不再吞错——失败经全局错误通道可见（busy 收尾不变）
                void props
                  .onConfirm()
                  .catch((e) => reportError(e, { context: "确认执行失败" }))
                  .finally(() => setBusy(false));
              }}
            >
              按建议名继续
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
