import ThemePreview from "./theme/ThemePreview";
import MainWorkbench from "./windows/MainWorkbench";
import QuickPanel from "./windows/QuickPanel";
import OverlayShot from "./windows/OverlayShot";
import PinWindow from "./windows/PinWindow";

/**
 * 窗口角色路由（docs/UI-PLAN.md U2-1）：
 * - main（默认）      → 主工作台
 * - quickpanel        → 剪切板快速面板（U3-5）
 * - theme-preview     → 主题基线页（U1-2）
 * - overlay           → 截图选区+标注覆盖层（M3）
 * - pin               → 贴图置顶窗口（M3）
 */
export default function App({ windowRole }: { windowRole: string }) {
  switch (windowRole) {
    case "quickpanel":
      return <QuickPanel />;
    case "theme-preview":
      return <ThemePreview />;
    case "overlay":
      return <OverlayShot />;
    case "pin":
      return <PinWindow />;
    default:
      return <MainWorkbench />;
  }
}
