import ThemePreview from "./theme/ThemePreview";
import MainWorkbench from "./windows/MainWorkbench";

/**
 * 窗口角色路由（docs/UI-PLAN.md U2-1 的雏形）：
 * - main（默认）      → 主工作台
 * - theme-preview     → 主题基线页（U1-2）
 * - 其余角色          → U2-1 扩展 quickpanel / overlay-* / pin-*
 */
export default function App({ windowRole }: { windowRole: string }) {
  switch (windowRole) {
    case "theme-preview":
      return <ThemePreview />;
    default:
      return <MainWorkbench />;
  }
}
