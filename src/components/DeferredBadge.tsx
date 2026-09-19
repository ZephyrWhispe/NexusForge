import { Badge, makeStyles, tokens, Tooltip } from "@fluentui/react-components";

/**
 * 延后范围可见化徽标（D-29 B0/T-B0-5，差距登记可见化轨道）：
 * 蓝图承诺但按 DECISIONS/批次排期延后交付的功能，在对应面板上挂一枚
 * outline 徽标，Tooltip 指出去处——用户看得见"这里将有什么、依据哪条决策延后"，
 * 而不是静默缺席或伪装成已实现的禁用按钮。
 */

const useStyles = makeStyles({
  badge: {
    cursor: "help",
    color: tokens.colorNeutralForeground3,
  },
});

export function deferredTooltipContent(label: string, decisionRef: string): string {
  return `${label}：本版本未交付（依据 ${decisionRef}）。出处见 docs/DECISIONS.md 与 docs/impl/09-blueprint-alignment.md 批次排期。`;
}

export function DeferredBadge({
  label,
  decisionRef,
}: {
  label: string;
  decisionRef: string;
}) {
  const styles = useStyles();
  return (
    <Tooltip relationship="label" content={deferredTooltipContent(label, decisionRef)}>
      {/* title = 原生悬浮兜底（无 Tooltip 交互上下文时仍可读到出处），与 Tooltip content 同源 */}
      <Badge appearance="outline" className={styles.badge} aria-disabled="true" title={deferredTooltipContent(label, decisionRef)}>
        {label} · 延后
      </Badge>
    </Tooltip>
  );
}

export default DeferredBadge;
