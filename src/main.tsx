import React from "react";
import ReactDOM from "react-dom/client";
import { FluentProvider } from "@fluentui/react-components";
import "./styles/global.css";
import App from "./App";
import Toaster from "./components/Toaster";
import ConfirmDialogHost from "./components/ConfirmDialog";
import { buildThemeSet } from "./theme/theme";
import { hostSystemAccent } from "./ipc/client";
import { reportError, isMonacoTeardownCanceled } from "./stores/notifications";
import { useSession, resolveIsLight } from "./stores/session";

/**
 * 窗口角色分发（docs/UI-PLAN.md U2-1 的雏形）：
 * Tauri 每个窗口加载本页并通过 ?w=<role> 区分角色。
 * 已支持：main（默认）、theme-preview（U1-2 主题基线页）。
 * 待扩展：quickpanel / overlay-* / pin-*（U2-1）。
 */
const role = new URLSearchParams(window.location.search).get("w") ?? "main";

// D-19：漏网异常统一进全局通道（日志 + 角标 + toast），不再无声消失在 console
window.addEventListener("unhandledrejection", (e) => {
  if (isMonacoTeardownCanceled(e.reason)) return; // monaco 销毁期内部噪声，见谓词注释
  reportError(e.reason, { context: "未处理的异步错误", dedupeKey: String(e.reason).slice(0, 120) });
});
window.addEventListener("error", (e) => {
  reportError(e.error ?? e.message, { context: "未捕获的前端错误", dedupeKey: String(e.message).slice(0, 120) });
});

function Root() {
  // D-14：明暗 = session store 显式模式优先，auto 跟随系统（matchMedia 实时监听，
  // OS 切换深浅色时所有窗口即时响应；跨窗口一致性由 session persist 的 storage 同步保证）
  const themeMode = useSession((s) => s.themeMode);
  const [systemPrefersLight, setSystemPrefersLight] = React.useState(
    () => window.matchMedia("(prefers-color-scheme: light)").matches,
  );
  // U1-3：默认 Windows 蓝先行渲染（避免白屏），系统强调色返回后热替换
  const [themeSet, setThemeSet] = React.useState(() => buildThemeSet(null));

  React.useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    const onChange = (e: MediaQueryListEvent) => setSystemPrefersLight(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  React.useEffect(() => {
    let alive = true;
    hostSystemAccent().then((accent) => {
      if (alive && accent) setThemeSet(buildThemeSet(accent));
    });
    return () => {
      alive = false;
    };
  }, []);

  const theme = resolveIsLight(themeMode, systemPrefersLight) ? themeSet.light : themeSet.dark;
  return (
    <FluentProvider theme={theme}>
      <App windowRole={role} />
      <Toaster />
      <ConfirmDialogHost />
    </FluentProvider>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
