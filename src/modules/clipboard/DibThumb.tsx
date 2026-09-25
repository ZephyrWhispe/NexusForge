import { useEffect, useState } from "react";
import { tokens } from "@fluentui/react-components";
import { clipboardGetImage } from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import { IN_TAURI } from "../../ipc/env";
import { dibToDataUrl } from "./dib";

/**
 * 图片条目缩略图（DIB → canvas → PNG dataURL）。
 * ClipboardPanel 与 QuickPanel 共用；解码失败显示占位块。
 *
 * PERF-09：
 * - LRU 缓存（按条目 id）——列表滚动时重挂载不再重发 IPC + 重解码；
 * - O(w·h) 逐像素解码移入 Worker（OffscreenCanvas；不可用时同步回退），
 *   大图不再阻塞主线程。
 */

/** 缩略图 dataURL LRU 缓存（有界；按 id 命中，超限淘汰最久未用） */
const THUMB_CACHE_MAX = 128;
const thumbCache = new Map<string, string>();
function cacheGet(id: string): string | undefined {
  const hit = thumbCache.get(id);
  if (hit !== undefined) {
    // LRU 触碰：删后重插使其成为"最近使用"
    thumbCache.delete(id);
    thumbCache.set(id, hit);
  }
  return hit;
}
function cachePut(id: string, url: string): void {
  thumbCache.delete(id);
  thumbCache.set(id, url);
  while (thumbCache.size > THUMB_CACHE_MAX) {
    const oldest = thumbCache.keys().next().value;
    if (oldest === undefined) break;
    thumbCache.delete(oldest);
  }
}

/** Worker 解码：返回 dataURL 或 null（不可用/失败由调用方回退同步路径） */
let dibWorker: Worker | null = null;
let workerBroken = false;
const pending = new Map<
  string,
  (url: string | null) => void
>();
function decodeInWorker(id: string, bin: Uint8Array): Promise<string | null> | null {
  if (workerBroken || !IN_TAURI) return null;
  try {
    if (!dibWorker) {
      dibWorker = new Worker(new URL("./dibWorker.ts", import.meta.url), { type: "module" });
      dibWorker.onmessage = (e: MessageEvent) => {
        const { id: rid, png } = e.data as { id: string; png: ArrayBuffer | null };
        const resolve = pending.get(rid);
        pending.delete(rid);
        resolve?.(png ? bytesToDataUrl(new Uint8Array(png)) : null);
      };
      dibWorker.onerror = () => {
        workerBroken = true; // 一次性降级：后续走同步
        for (const resolve of pending.values()) resolve(null);
        pending.clear();
      };
    }
    return new Promise((resolve) => {
      pending.set(id, resolve);
      // 复制一份进 transfer（调用方的 bin 仍归调用方所有）
      const copy = bin.slice();
      dibWorker!.postMessage({ id, bytes: copy.buffer }, [copy.buffer]);
    });
  } catch {
    workerBroken = true;
    return null;
  }
}

function bytesToDataUrl(png: Uint8Array): string {
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < png.length; i += CHUNK) {
    bin += String.fromCharCode(...png.subarray(i, i + CHUNK));
  }
  return `data:image/png;base64,${btoa(bin)}`;
}
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
    // PERF-09：缓存命中零 IPC 零解码
    const cached = cacheGet(id);
    if (cached) {
      setSrc(cached);
      return;
    }
    let alive = true;
    const commit = (url: string | null) => {
      if (!alive) return;
      if (url) {
        cachePut(id, url);
        setSrc(url);
      }
    };
    clipboardGetImage(id)
      .then(async (b64) => {
        const bin = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        // Worker 优先（不阻塞主线程）；不可用/失败回退同步
        const viaWorker = await decodeInWorker(id, bin);
        if (viaWorker !== null) {
          commit(viaWorker);
          return;
        }
        commit(dibToDataUrl(bin));
      })
      .catch((e) =>
        reportError(e, { context: "图片缩略图解码失败", dedupeKey: "dib-thumb", toast: false }),
      );
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
