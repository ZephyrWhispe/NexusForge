import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles, tokens } from "@fluentui/react-components";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { PhysicalSize } from "@tauri-apps/api/dpi";
import { screenshotPinGet, screenshotPinUpdate, screenshotPinClose, type PinDataDto } from "../ipc/client";

/**
 * 贴图置顶窗口（docs/impl/03 P6）：
 * 滚轮缩放（0.2–5.0）、Alt+滚轮透明度（0.2–1.0）、按住拖拽移动、双击关闭。
 * 缩放/透明度变化即时持久化（pins.json），重启由主窗口 restorePins() 恢复。
 */
const useStyles = makeStyles({
  root: {
    width: "100vw",
    height: "100vh",
    overflow: "hidden",
    cursor: "grab",
    ":active": { cursor: "grabbing" },
  },
  img: {
    display: "block",
    width: "100%",
    height: "100%",
    objectFit: "fill",
  },
  hint: {
    position: "absolute",
    bottom: "6px",
    right: "10px",
    padding: "2px 8px",
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: "rgba(28,28,30,0.85)",
    color: "#fff",
    fontSize: tokens.fontSizeBase100,
    pointerEvents: "none",
    opacity: 0,
    transitionProperty: "opacity",
    transitionDuration: "300ms",
  },
  hintVisible: {
    opacity: 1,
  },
});

export default function PinWindow() {
  const styles = useStyles();
  const [pin, setPin] = useState<PinDataDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hint, setHint] = useState(false);
  const pinRef = useRef<PinDataDto | null>(null);
  const hintTimer = useRef<number | null>(null);

  useEffect(() => {
    const id = new URLSearchParams(window.location.search).get("pin");
    if (!id) {
      setError("缺少贴图参数");
      return;
    }
    screenshotPinGet(id)
      .then((p) => {
        pinRef.current = p;
        setPin(p);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const flashHint = () => {
    setHint(true);
    if (hintTimer.current) window.clearTimeout(hintTimer.current);
    hintTimer.current = window.setTimeout(() => setHint(false), 900);
  };

  /** 关闭贴图：删记录 + 关窗（Esc/双击/右键共用） */
  const closePin = useCallback(() => {
    void screenshotPinClose(pinRef.current?.id ?? "")
      .catch(() => undefined)
      .finally(() => getCurrentWindow().close());
  }, []);

  // Esc 关闭（快捷退出大贴图）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        closePin();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [closePin]);

  // 首次打开 3 秒操作提示（交互不可发现是贴图最常见困惑）
  const [intro, setIntro] = useState(true);
  useEffect(() => {
    if (!pin) return;
    const t = window.setTimeout(() => setIntro(false), 3000);
    return () => window.clearTimeout(t);
  }, [pin]);

  /** 按下起点 + 拖拽标志：移动超阈值才进入 OS 拖拽循环，
   *  否则 startDragging 的模态循环会吞掉 click/dblclick，双击关闭失效 */
  const downPos = useRef<{ x: number; y: number } | null>(null);
  const dragging = useRef(false);

  const applyZoom = async (zoom: number) => {
    const p = pinRef.current;
    if (!p) return;
    const next = Math.min(5, Math.max(0.2, zoom));
    const updated = { ...p, zoom: next };
    pinRef.current = updated;
    setPin(updated);
    try {
      await getCurrentWindow().setSize(
        new PhysicalSize(Math.round(p.width * next), Math.round(p.height * next)),
      );
    } catch {
      // 窗口可能在关闭流程中
    }
    void screenshotPinUpdate(p.id, next, p.opacity).catch(() => undefined);
    flashHint();
  };

  const applyOpacity = async (opacity: number) => {
    const p = pinRef.current;
    if (!p) return;
    const next = Math.min(1, Math.max(0.2, opacity));
    const updated = { ...p, opacity: next };
    pinRef.current = updated;
    setPin(updated);
    void screenshotPinUpdate(p.id, p.zoom, next).catch(() => undefined);
    flashHint();
  };

  if (error) {
    return (
      <div style={{ padding: 12, color: tokens.colorNeutralForeground1 }}>
        <div>{error}</div>
        <button onClick={() => getCurrentWindow().close()}>关闭</button>
      </div>
    );
  }

  if (!pin) return null;

  return (
    <div
      className={styles.root}
      onWheel={(e) => {
        e.preventDefault();
        if (e.altKey) {
          void applyOpacity(pinRef.current!.opacity - e.deltaY * 0.001);
        } else {
          void applyZoom(pinRef.current!.zoom * (e.deltaY < 0 ? 1.1 : 0.9));
        }
      }}
      onMouseDown={(e) => {
        if (e.button === 0) {
          downPos.current = { x: e.clientX, y: e.clientY };
          dragging.current = false;
        }
      }}
      onMouseMove={(e) => {
        // 超过 4px 才进入 OS 拖拽循环（保证双击/普通点击不被吞）
        if (downPos.current && !dragging.current) {
          const dx = e.clientX - downPos.current.x;
          const dy = e.clientY - downPos.current.y;
          if (Math.hypot(dx, dy) > 4) {
            dragging.current = true;
            void getCurrentWindow().startDragging();
          }
        }
      }}
      onMouseUp={() => {
        downPos.current = null;
        dragging.current = false;
      }}
      onDoubleClick={closePin}
      onContextMenu={(e) => {
        e.preventDefault();
        closePin();
      }}
      title="滚轮缩放 · Alt+滚轮透明度 · 拖拽移动 · 双击/右键/Esc 关闭"
    >
      <img
        className={styles.img}
        src={`data:image/png;base64,${pin.png_b64}`}
        alt="pin"
        draggable={false}
        style={{ opacity: pin.opacity }}
      />
      <div className={`${styles.hint} ${hint || intro ? styles.hintVisible : ""}`}>
        {intro
          ? "拖拽移动 · 滚轮缩放 · Alt+滚轮透明度 · 双击/Esc/右键关闭"
          : `${Math.round(pin.zoom * 100)}% · ${Math.round(pin.opacity * 100)}%`}
      </div>
    </div>
  );
}
