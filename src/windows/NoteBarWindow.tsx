import { useCallback, useState } from "react";
import { makeStyles, tokens, Text } from "@fluentui/react-components";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { desktopNoteAdd } from "../ipc/client";

/**
 * 快速速记条（docs/impl/05 D4）：Ctrl+Alt+N 全局呼出的顶部小条。
 * 输入即存：#标签、明天/周几提醒解析在 core 内；Enter 保存并隐藏，Esc 隐藏。
 */
const useStyles = makeStyles({
  root: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusXLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    boxShadow: tokens.shadow16,
  },
  input: {
    flex: 1,
    fontSize: tokens.fontSizeBase300,
    padding: "6px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
  },
  hint: { flex: "none", color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase100 },
  ok: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
});

export default function NoteBarWindow() {
  const styles = useStyles();
  const [text, setText] = useState("");
  const [saved, setSaved] = useState(false);

  const save = useCallback(async () => {
    const content = text.trim();
    if (!content) {
      await getCurrentWindow().hide();
      return;
    }
    try {
      await desktopNoteAdd(content);
      setSaved(true);
      setText("");
      setTimeout(async () => {
        setSaved(false);
        await getCurrentWindow().hide();
      }, 700);
    } catch {
      // 保存失败保留输入（不打断）
    }
  }, [text]);

  return (
    <div className={styles.root}>
      <Text size={200} weight="semibold">速记</Text>
      <input
        className={styles.input}
        placeholder="记点什么… 支持 #标签、“明天/周几 X点”提醒"
        value={text}
        autoFocus
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
          if (e.key === "Escape") void getCurrentWindow().hide();
        }}
      />
      {saved ? (
        <span className={styles.ok}>已保存</span>
      ) : (
        <span className={styles.hint}>Enter 保存 · Esc 关闭</span>
      )}
    </div>
  );
}
