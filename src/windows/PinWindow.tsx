import { useEffect, useRef, useState } from "react";
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
        if (e.button === 0) void getCurrentWindow().startDragging();
      }}
      onDoubleClick={() => {
        void screenshotPinClose(pin.id)
          .catch(() => undefined)
          .finally(() => getCurrentWindow().close());
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        void screenshotPinClose(pin.id)
          .catch(() => undefined)
          .finally(() => getCurrentWindow().close());
      }}
      title="滚轮缩放 · Alt+滚轮透明度 · 拖拽移动 · 双击/右键关闭"
    >
      <img
        className={styles.img}
        src={`data:image/png;base64,${pin.png_b64}`}
        alt="pin"
        draggable={false}
        style={{ opacity: pin.opacity }}
      />
      <div className={`${styles.hint} ${hint ? styles.hintVisible : ""}`}>
        {Math.round(pin.zoom * 100)}% · {Math.round(pin.opacity * 100)}%
      </div>
    </div>
  );
}
