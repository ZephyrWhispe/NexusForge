import { lazy, Suspense, type ReactNode } from "react";
import MainWorkbench from "./windows/MainWorkbench";

/**
 * 窗口角色路由（docs/UI-PLAN.md U2-1）：
 * - main（默认）      → 主工作台
 * - quickpanel        → 剪切板快速面板（U3-5）
 * - theme-preview     → 主题基线页（U1-2）
 * - overlay           → 截图选区+标注覆盖层（M3）
 * - pin               → 贴图置顶窗口（M3）
 * - launcher          → 快速启动器（M8 D1）
 * - notebar           → 快速速记条（M8 D4）
 *
 * D-43 C4b：除 main 外全部改 lazy。每个窗口只渲一枚角色，静态导入却把七枚页面
 * 一起塞进主窗首屏的 index chunk（OverlayShot 63.7kB＋四枚 aux 窗与主题基线页 ≈90kB 源码），
 * 而覆盖层是启动即预热的隐藏窗（overlayController.prewarmOverlay，"内容就绪才 show"），
 * 它的 chunk 装载发生在后台预热期、不在热键链路上——拆出去主窗首屏少载、aux 窗多一次本地请求。
 * fallback 恒 null：这些窗口在内容就绪前本就不可见，spinner 反而会在覆盖层里闪一下。
 */
const QuickPanel = lazy(() => import("./windows/QuickPanel"));
const ThemePreview = lazy(() => import("./theme/ThemePreview"));
const OverlayShot = lazy(() => import("./windows/OverlayShot"));
const PinWindow = lazy(() => import("./windows/PinWindow"));
const LauncherWindow = lazy(() => import("./windows/LauncherWindow"));
const NoteBarWindow = lazy(() => import("./windows/NoteBarWindow"));

function route(windowRole: string): ReactNode {
  switch (windowRole) {
    case "quickpanel":
      return <QuickPanel />;
    case "theme-preview":
      return <ThemePreview />;
    case "overlay":
      return <OverlayShot />;
    case "pin":
      return <PinWindow />;
    case "launcher":
      return <LauncherWindow />;
    case "notebar":
      return <NoteBarWindow />;
    default:
      return <MainWorkbench />;
  }
}

export default function App({ windowRole }: { windowRole: string }) {
  return <Suspense fallback={null}>{route(windowRole)}</Suspense>;
}
