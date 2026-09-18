import type { KeyboardEvent } from "react";

/**
 * 可点击 div 的键盘等价（审查 D-17：jsx-a11y "点击无键盘等价" 类缺陷的统一点火器）。
 * 配合字面量 role/tabIndex 使用：`role="button" tabIndex={0} onKeyDown={keyActivate(fn)}`。
 * 写成属性工厂会被 spread——jsx-a11y 静态检查看不见 spread，规则不会放行，故保留字面量。
 * D-18 组件基线（Section/Tabs）落地后，列表行类站点应收敛到共享组件。
 */
export function keyActivate(fn: () => void): (e: KeyboardEvent) => void {
  return (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
      fn();
    }
  };
}
