import ThemePreview from "./theme/ThemePreview";
import MainWorkbench from "./windows/MainWorkbench";
import QuickPanel from "./windows/QuickPanel";

/**
 * 窗口角色路由（docs/UI-PLAN.md U2-1）：
 * - main（默认）      → 主工作台
 * - quickpanel        → 剪切板快速面板（U3-5）
 * - theme-preview     → 主题基线页（U1-2）
 * - 其余角色          → U2-1 扩展 overlay-* / pin-*
 */
export default function App({ windowRole }: { windowRole: string }) {
  switch (windowRole) {
    case "quickpanel":
      return <QuickPanel />;
    case "theme-preview":
      return <ThemePreview />;
    default:
      return <MainWorkbench />;
  }
}
