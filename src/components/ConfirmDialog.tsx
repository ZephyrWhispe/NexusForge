import {
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
import { impactLines, useConfirmStore } from "../stores/confirm";

/**
 * 全局二次确认对话框宿主（审查 D-18）：基于 Fluent Dialog，自带焦点陷阱
 * 与 Esc 关闭（onOpenChange → 取消当前队列项）。挂载于 main.tsx Root()，
 * 与 Toaster 并列，覆盖所有 ?w= 窗口角色；面板经 confirmAction() 排队。
 */

const useStyles = makeStyles({
  impact: {
    display: "block",
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
  },
  detail: {
    display: "block",
    marginTop: "6px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  command: {
    display: "block",
    marginTop: "10px",
    padding: "8px 12px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground2,
    fontFamily: "Consolas, monospace",
    fontSize: tokens.fontSizeBase200,
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
  },
  actions: { display: "flex", gap: "8px", justifyContent: "flex-end" },
  // 破坏性主按钮 = 红色（当前安装版 Fluent Button 无 overflow appearance）
  danger: {
    backgroundColor: tokens.colorPaletteRedBackground1,
    color: tokens.colorPaletteRedForeground1,
  },
});

export default function ConfirmDialogHost() {
  const styles = useStyles();
  const head = useConfirmStore((s) => s.queue[0]);
  const settleHead = useConfirmStore((s) => s.settleHead);

  const danger = head?.danger !== false;
  return (
    <Dialog open={head !== undefined} onOpenChange={() => settleHead(false)}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{head?.title ?? ""}</DialogTitle>
          <DialogContent>
            {head &&
              impactLines(head).map((line) => (
                <span key={line} className={styles.impact}>
                  {line}
                </span>
              ))}
            {head?.detail && <span className={styles.detail}>{head.detail}</span>}
            {head?.command && (
              <>
                <code className={styles.command}>{head.command}</code>
                <span className={styles.detail}>确认执行以上确切命令？</span>
              </>
            )}
          </DialogContent>
          <DialogActions className={styles.actions}>
            <Button appearance="subtle" onClick={() => settleHead(false)}>
              {head?.cancelLabel ?? "取消"}
            </Button>
            <Button
              appearance="primary"
              className={danger ? styles.danger : undefined}
              onClick={() => settleHead(true)}
            >
              {head?.confirmLabel ?? "确认执行"}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
