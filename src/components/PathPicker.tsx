import { Button, Input, makeStyles } from "@fluentui/react-components";
import { useId } from "react";

import InlineError from "./InlineError";
import { SPACING, TIER_W } from "./nfTiers";

/**
 * 路径选择器（00 规范 3 节：PathPicker = Input＋浏览按钮，L 档宽，"禁逼用户手打绝对路径"）。
 * 对话框由调用方自持（`onBrowse` 回调化），本件不引 window 层依赖。
 * 规范 4-3：字段级 inline 错误经 `aria-describedby` 与输入框关联。
 */

const useStyles = makeStyles({
  root: { display: "flex", flexDirection: "column", gap: SPACING.x4 },
  line: { alignItems: "center", display: "flex", gap: SPACING.x8 },
  input: { width: TIER_W.l },
});

export default function PathPicker({
  value,
  onChange,
  onBrowse,
  placeholder,
  disabled,
  error,
  label,
}: {
  value: string;
  onChange: (next: string) => void;
  onBrowse: () => void;
  placeholder?: string;
  disabled?: boolean;
  error?: string | null;
  label?: string;
}) {
  const styles = useStyles();
  const errorId = useId();
  return (
    <div className={styles.root}>
      <div className={styles.line}>
        <Input
          value={value}
          onChange={(_e, data) => onChange(data.value)}
          placeholder={placeholder}
          disabled={disabled}
          className={styles.input}
          aria-label={label}
          {...(error ? { "aria-describedby": errorId, validationState: "error" as const } : {})}
        />
        <Button onClick={onBrowse} disabled={disabled}>
          浏览
        </Button>
      </div>
      <InlineError text={error} id={errorId} />
    </div>
  );
}
