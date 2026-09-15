import { useEffect, useState } from "react";
import { makeStyles, tokens, Text } from "@fluentui/react-components";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { clipboardSearch, clipboardPaste, type ClipEntry } from "../ipc/client";
import DibThumb from "../modules/clipboard/DibThumb";

/**
 * 剪切板快速面板（docs/UI-PLAN.md U3-5）：独立置顶小窗。
 * 交互：数字键 1–9 直选粘贴；Esc 隐藏；粘贴后自动隐藏。
 */
const useStyles = makeStyles({
  root: {
    height: "100vh",
    display: "flex",
    flexDirection: "column",
    borderRadius: tokens.borderRadiusXLarge,
    overflow: "hidden",
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  head: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "12px 16px 6px",
    fontWeight: tokens.fontWeightSemibold,
  },
  hint: {
    marginLeft: "auto",
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground3,
  },
  list: { padding: "4px 8px 10px", overflowY: "auto" },
  item: {
    display: "flex",
    alignItems: "center",
    gap: "12px",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusLarge,
    cursor: "pointer",
    ":hover": { backgroundColor: tokens.colorNeutralBackground3Hover },
  },
  num: {
    flex: "none",
    width: "22px",
    height: "22px",
    borderRadius: tokens.borderRadiusSmall,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    display: "grid",
    placeItems: "center",
    fontSize: tokens.fontSizeBase100,
    color: tokens.colorNeutralForeground2,
  },
  text: {
    flex: 1,
    minWidth: 0,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
    color: tokens.colorNeutralForeground2,
  },
  time: { fontSize: tokens.fontSizeBase100, color: tokens.colorNeutralForeground3 },
  secret: { color: tokens.colorPaletteDarkOrangeForeground1 },
});

function hideWindow() {
  void getCurrentWindow().hide();
}

export default function QuickPanel() {
  const styles = useStyles();
  const [items, setItems] = useState<ClipEntry[]>([]);

  useEffect(() => {
    clipboardSearch({ size: 9 })
      .then((p) => setItems(p.items))
      .catch(() => setItems([]));
    const t = window.setInterval(
      () => clipboardSearch({ size: 9 }).then((p) => setItems(p.items)).catch(() => {}),
      1500,
    );
    return () => window.clearInterval(t);
  }, []);

  useEffect(() => {
    const pick = (n: number) => {
      const entry = items[n - 1];
      if (!entry) return;
      clipboardPaste(entry.id).finally(hideWindow);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") return hideWindow();
      const n = Number(e.key);
      if (n >= 1 && n <= 9) return pick(n);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [items]);

  return (
    <div className={styles.root} data-tauri-drag-region>
      <div className={styles.head}>
        剪切板
        <Text className={styles.hint}>数字键直选 · Esc 关闭</Text>
      </div>
      <div className={styles.list}>
        {items.length === 0 && (
          <Text style={{ padding: "12px 16px", color: tokens.colorNeutralForeground3 }}>
            暂无历史 — 复制任意内容后这里会出现最近的 9 条
          </Text>
        )}
        {items.map((e, i) => (
          <div
            key={e.id}
            className={styles.item}
            onClick={() => clipboardPaste(e.id).finally(hideWindow)}
          >
            <span className={styles.num}>{i + 1}</span>
            {e.content_type === "image" && <DibThumb id={e.id} width={36} height={22} />}
            <span className={`${styles.text} ${e.secret ? styles.secret : ""}`}>
              {e.secret ? "[敏感内容] 已加密存储" : e.preview}
            </span>
            <span className={styles.time}>{e.usage_count > 0 ? `×${e.usage_count}` : ""}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
