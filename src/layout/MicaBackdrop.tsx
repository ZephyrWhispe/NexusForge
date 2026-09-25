import { useEffect, useState } from "react";
import { makeStyles } from "@fluentui/react-components";
import { hostCapabilities } from "../ipc/client";
import { IN_TAURI } from "../ipc/env";

/**
 * Mica 材质背景（docs/UI-PLAN.md U1-4 + COR-26）。
 *
 * - Win11（build ≥ 22000）非远程会话：透明基底由 tauri windowEffects=mica 提供，
 *   本组件仅负责把页面基底设为透明，让系统材质透出；
 * - Win10 / RDP / 部分 VM / 浏览器预览：Mica 不可用 → 渐变回退底
 *   （旧路径 `if (IN_TAURI) return null` 在透明窗上直接透出壁纸，可读性崩坏）。
 *   能力探测经 hostCapabilities 一次取回（探测未落定前先铺回退底，避免闪透）。
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

export default function MicaBackdrop() {
  const styles = useStyles();
  const light = window.matchMedia("(prefers-color-scheme: light)").matches;
  // undefined = 探测中（按不支持处理：先铺可读回退底，探测真值到了再切换）
  const [mica, setMica] = useState<boolean | undefined>(undefined);

  useEffect(() => {
    if (!IN_TAURI) return;
    let cancelled = false;
    hostCapabilities()
      .then((caps) => {
        if (!cancelled) setMica(caps.mica);
      })
      .catch(() => {
        if (!cancelled) setMica(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (IN_TAURI && mica === true) return null;
  return <div className={`${styles.micaFallback} ${light ? styles.lightFallback : ""}`} aria-hidden />;
}
