/**
 * PERF-09 ③：DIB 解码 Worker——8192² 级大图的 O(w·h) 逐像素循环与 PNG 编码
 * 移出主线程（主线程回退：OffscreenCanvas 不可用时 DibThumb 走同步路径）。
 * 消息协议：入 {id, bytes(ArrayBuffer, transferable)}；出 {id, png(ArrayBuffer|null)}。
 * PNG 字节回主线程后 base64 封 data:（CSP img-src 未放行 blob:，禁 worker 直出 URL）。
 */
import { decodeDibToRgba, rgbaToPngBytes } from "./dib";

self.onmessage = (e: MessageEvent) => {
  const { id, bytes } = e.data as { id: string; bytes: ArrayBuffer };
  const respond = (png: ArrayBuffer | null) => {
    (self as unknown as Worker).postMessage({ id, png });
  };
  try {
    const decoded = decodeDibToRgba(new Uint8Array(bytes));
    if (!decoded) {
      respond(null);
      return;
    }
    const png = rgbaToPngBytes(decoded.w, decoded.h, decoded.rgba);
    if (!png) {
      respond(null);
      return;
    }
    png
      .then((b) => respond(b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer))
      .catch(() => respond(null));
  } catch {
    respond(null);
  }
};
