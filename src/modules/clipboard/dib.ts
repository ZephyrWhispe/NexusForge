/**
 * DIB（BITMAPINFO）→ PNG dataURL 解码（docs/impl/02 前端预览）。
 * 支持 32bpp（BGRA，含 BI_BITFIELDS 标准掩码）与 24bpp（BGR）；
 * 其余位深/压缩格式返回 null（显示占位）。
 */
export function dibToDataUrl(bytes: Uint8Array): string | null {
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

  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  const img = ctx.createImageData(w, h);
  const bottomUp = hRaw > 0;

  if (bpp === 32) {
    for (let y = 0; y < h; y++) {
      const sy = bottomUp ? h - 1 - y : y;
      for (let x = 0; x < w; x++) {
        const o = pixOff + (sy * w + x) * 4;
        const di = (y * w + x) * 4;
        img.data[di] = bytes[o + 2];
        img.data[di + 1] = bytes[o + 1];
        img.data[di + 2] = bytes[o];
        img.data[di + 3] = bytes[o + 3];
      }
    }
  } else if (bpp === 24) {
    const stride = ((w * 24 + 31) & ~31) >> 3;
    for (let y = 0; y < h; y++) {
      const sy = bottomUp ? h - 1 - y : y;
      for (let x = 0; x < w; x++) {
        const o = pixOff + sy * stride + x * 3;
        const di = (y * w + x) * 4;
        img.data[di] = bytes[o + 2];
        img.data[di + 1] = bytes[o + 1];
        img.data[di + 2] = bytes[o];
        img.data[di + 3] = 255;
      }
    }
  } else {
    return null;
  }

  ctx.putImageData(img, 0, 0);
  return canvas.toDataURL("image/png");
}
