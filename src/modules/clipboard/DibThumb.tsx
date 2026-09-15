import { useEffect, useState } from "react";
import { tokens } from "@fluentui/react-components";
import { clipboardGetImage } from "../../ipc/client";
import { IN_TAURI } from "../../ipc/env";
import { dibToDataUrl } from "./dib";

/**
 * 图片条目缩略图（DIB → canvas → PNG dataURL）。
 * ClipboardPanel 与 QuickPanel 共用；解码失败显示占位块。
 */
export default function DibThumb({
  id,
  width = 84,
  height = 52,
}: {
  id: string;
  width?: number;
  height?: number;
}) {
  const [src, setSrc] = useState<string | null>(null);

  useEffect(() => {
    if (!IN_TAURI) return;
    let alive = true;
    clipboardGetImage(id)
      .then((b64) => {
        const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        if (alive) setSrc(dibToDataUrl(bin));
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [id]);

  if (src) {
    return (
      <img
        src={src}
        alt="剪贴板图片"
        style={{
          flex: "none",
          width,
          height,
          objectFit: "cover",
          borderRadius: tokens.borderRadiusMedium,
          border: `1px solid ${tokens.colorNeutralStroke1}`,
        }}
      />
    );
  }
  return (
    <div
      style={{
        flex: "none",
        width,
        height,
        borderRadius: tokens.borderRadiusMedium,
        background: tokens.colorNeutralBackground3,
        display: "grid",
        placeItems: "center",
        color: tokens.colorNeutralForeground3,
        fontSize: tokens.fontSizeBase100,
      }}
    >
      图片
    </div>
  );
}
