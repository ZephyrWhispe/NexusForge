import { makeStyles } from "@fluentui/react-components";

/**
 * Mica 材质背景（docs/UI-PLAN.md U1-4）。
 *
 * - Tauri 窗口：透明背景由 tauri.conf.json 的 windowEffects=mica 提供，
 *   本组件仅负责将页面基底设为透明，让系统材质透出；
 * - 浏览器（tauri dev 之外的 HMR 预览）：无 Mica，回退为 demo 同款渐变近似。
 */
const useStyles = makeStyles({
  micaFallback: {
    position: "fixed",
    inset: "0",
    zIndex: "-1",
    background:
      "linear-gradient(135deg, rgba(28,31,38,0.96) 0%, rgba(32,36,46,0.94) 55%, rgba(27,32,40,0.96) 100%)",
  },
  lightFallback: {
    position: "fixed",
    inset: "0",
    zIndex: "-1",
    background:
      "linear-gradient(135deg, rgba(238,242,247,0.96) 0%, rgba(232,237,245,0.94) 60%, rgba(227,235,246,0.96) 100%)",
  },
});

const IN_TAURI = "__TAURI_INTERNALS__" in window;

export default function MicaBackdrop() {
  const styles = useStyles();
  const light = window.matchMedia("(prefers-color-scheme: light)").matches;
  // Tauri：真实 Mica 由系统合成，页面只留透明底
  if (IN_TAURI) return null;
  // 浏览器预览：渐变近似
  return <div className={`${styles.micaFallback} ${light ? styles.lightFallback : ""}`} aria-hidden />;
}
