import { makeStyles, tokens } from "@fluentui/react-components";
import { useNotifications, type NoteKind } from "../stores/notifications";

/**
 * 全局 toast 宿主（docs/DESIGN.md §8.2，审查 D-19；视觉规格 demo/index.html Toast）。
 * 挂载于 main.tsx Root()，所有 ?w= 窗口角色共享同一通道。
 */

const useStyles = makeStyles({
  root: {
    position: "fixed",
    right: "16px",
    bottom: "40px",
    display: "flex",
    flexDirection: "column",
    gap: "10px",
    zIndex: 1000, // 高于 vault 模态（100）与一切面板内浮层
    pointerEvents: "none",
  },
  toast: {
    display: "flex",
    gap: "10px",
    alignItems: "center",
    width: "320px",
    padding: "12px 14px",
    borderRadius: "10px",
    backgroundColor: tokens.colorNeutralBackground1,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    boxShadow: tokens.shadow16,
    pointerEvents: "auto",
    animation: "nf-toast-in .22s ease both",
  },
  icon: {
    flex: "none",
    width: "20px",
    textAlign: "center",
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightBold,
  },
  iconErr: { color: tokens.colorPaletteRedForeground1 },
  iconWarn: { color: tokens.colorPaletteDarkOrangeForeground1 },
  iconOk: { color: tokens.colorPaletteGreenForeground1 },
  iconInfo: { color: tokens.colorBrandForeground1 },
  text: { flex: 1, minWidth: 0 },
  title: {
    display: "block",
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
  },
  body: {
    display: "block",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    wordBreak: "break-word",
  },
  close: {
    flex: "none",
    marginLeft: "auto",
    border: "none",
    background: "transparent",
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
    cursor: "pointer",
    padding: "2px 4px",
  },
});

const ICON: Record<NoteKind, { glyph: string; cls: "iconErr" | "iconWarn" | "iconOk" | "iconInfo" }> = {
  error: { glyph: "!", cls: "iconErr" },
  warn: { glyph: "!", cls: "iconWarn" },
  success: { glyph: "✓", cls: "iconOk" },
  info: { glyph: "i", cls: "iconInfo" },
};

export default function Toaster() {
  const styles = useStyles();
  const notes = useNotifications((s) => s.notes);
  const visible = useNotifications((s) => s.visible);
  const dismiss = useNotifications((s) => s.dismiss);

  const shown = visible
    .map((id) => notes.find((n) => n.id === id))
    .filter((n): n is NonNullable<typeof n> => n !== undefined);
  if (shown.length === 0) return null;

  return (
    <div className={styles.root} role="status" aria-live="polite">
      <style>{"@keyframes nf-toast-in{from{opacity:0;transform:translateX(24px)}to{opacity:1;transform:none}}"}</style>
      {shown.map((n) => {
        const ico = ICON[n.kind];
        return (
          <div
            key={n.id}
            className={styles.toast}
            {...(n.kind === "error" ? { role: "alert", "aria-live": "assertive" } : {})}
          >
            <span className={`${styles.icon} ${styles[ico.cls]}`}>{ico.glyph}</span>
            <span className={styles.text}>
              <b className={styles.title}>{n.title}</b>
              {n.body && <span className={styles.body}>{n.body}</span>}
            </span>
            <button
              className={styles.close}
              aria-label="关闭"
              onClick={() => dismiss(n.id)}
            >
              ✕
            </button>
          </div>
        );
      })}
    </div>
  );
}
