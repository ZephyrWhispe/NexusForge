import { makeStyles, tokens, Text } from "@fluentui/react-components";

/**
 * 行内提示基线（审查 D-18）：`.error`/`.ok` 文案样式此前 12+ 份重复。
 * text 为空时渲染 null，调用方免去三元。
 */

const useStyles = makeStyles({
  error: { color: tokens.colorPaletteRedForeground1, fontSize: tokens.fontSizeBase200 },
  success: { color: tokens.colorPaletteGreenForeground1, fontSize: tokens.fontSizeBase200 },
});

export default function InlineError({
  text,
  tone = "error",
}: {
  text?: string | null;
  tone?: "error" | "success";
}) {
  const styles = useStyles();
  if (!text) return null;
  return (
    <Text
      className={tone === "error" ? styles.error : styles.success}
      {...(tone === "error" ? { role: "alert", "aria-live": "assertive" } : {})}
    >
      {text}
    </Text>
  );
}
