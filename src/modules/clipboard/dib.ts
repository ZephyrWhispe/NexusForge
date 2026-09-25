/**
 * PERF-09 ①：DIB 头解析 + 像素搬运（纯函数，主线程与 Worker 共用一份实现）。
 * 返回 RGBA 像素（已翻转为上-下序）与尺寸；不触碰 DOM，可在 Worker 内运行。
 */
export function decodeDibToRgba(
  bytes: Uint8Array,
): { w: number; h: number; rgba: Uint8ClampedArray } | null {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 40) return null;
  const biSize = dv.getUint32(0, true);
  if (biSize < 40) return null;
  const w = dv.getInt32(4, true);
  const hRaw = dv.getInt32(8, true);
  const bpp = dv.getUint16(14, true);
  const comp = dv.getUint32(16, true);
  if (comp !== 0 && comp !== 3) return null; // 仅 BI_RGB / BI_BITFIELDS
  const h = Math.abs(hRaw);
  if (w <= 0 || h <= 0 || w > 8192 || h > 8192) return null;

  let pixOff = biSize + (comp === 3 ? 12 : 0);
  if (bpp <= 8) {
    const colors = dv.getUint32(32, true) || 1 << bpp;
    pixOff += colors * 4;
  }

  const img = new Uint8ClampedArray(w * h * 4);
  const bottomUp = hRaw > 0;

  if (bpp === 32) {
    for (let y = 0; y < h; y++) {
      const sy = bottomUp ? h - 1 - y : y;
      const srcRow = pixOff + sy * w * 4;
      const dstRow = y * w * 4;
      for (let x = 0; x < w; x++) {
        const o = srcRow + x * 4;
        const di = dstRow + x * 4;
        img[di] = bytes[o + 2];
        img[di + 1] = bytes[o + 1];
        img[di + 2] = bytes[o];
        img[di + 3] = bytes[o + 3];
      }
    }
  } else if (bpp === 24) {
    const stride = ((w * 24 + 31) & ~31) >> 3;
    for (let y = 0; y < h; y++) {
      const sy = bottomUp ? h - 1 - y : y;
      const srcRow = pixOff + sy * stride;
      const dstRow = y * w * 4;
      for (let x = 0; x < w; x++) {
        const o = srcRow + x * 3;
        const di = dstRow + x * 4;
        img[di] = bytes[o + 2];
        img[di + 1] = bytes[o + 1];
        img[di + 2] = bytes[o];
        img[di + 3] = 255;
      }
    }
  } else {
    return null;
  }
  return { w, h, rgba: img };
}

/**
 * PERF-09 ②：RGBA → PNG 字节（Worker 内经 OffscreenCanvas；不产 blob: URL——
 * CSP img-src 未放行 blob:，主线程以 data: 封装）。
 */
export function rgbaToPngBytes(
  w: number,
  h: number,
  rgba: Uint8ClampedArray,
): Promise<Uint8Array> | null {
  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.putImageData(new ImageData(rgba, w, h), 0, 0);
  return canvas
    .convertToBlob({ type: "image/png" })
    .then((blob) => blob.arrayBuffer())
    .then((buf) => new Uint8Array(buf));
}

/**
 * DIB（BITMAPINFO）→ PNG dataURL 解码（docs/impl/02 前端预览）。
 * 支持 32bpp（BGRA，含 BI_BITFIELDS 标准掩码）与 24bpp（BGR）；
 * 其余位深/压缩格式返回 null（显示占位）。
 */
export function dibToDataUrl(bytes: Uint8Array): string | null {
  const decoded = decodeDibToRgba(bytes);
  if (!decoded) return null;
  const { w, h, rgba } = decoded;
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.putImageData(new ImageData(rgba, w, h), 0, 0);
  return canvas.toDataURL("image/png");
}
