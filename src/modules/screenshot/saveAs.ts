/**
 * 浏览器侧"另存为"（09 §9.2 T-B4-7）。
 *
 * 为什么住前端而不是加一条 Rust 命令：本仓不挂 dialog/fs 插件（`src/ipc/client.ts`
 * 里那句"全仓无 dialog/fs 插件"的注释是同一事实），任务书这行又明写"无新命令、
 * 无 ACL 变化"，所以另存为只能是浏览器自己的 canvas 重编码 + `<a download>`。
 * 代价如实记在这里，不留"看起来和设置里那条路一样"的错觉：
 * ① 落盘位置由浏览器下载目录决定，不是设置里的 `save_dir`；
 * ② 不消费配置里的 `quality`——`canvas.toBlob` 的质量是 0..1 且面板读不到模块配置，
 *    硬换算就是假接线，所以这里干脆不传质量参（用浏览器默认）。
 * 磁盘写侧的格式与质量真源仍是 `util::encode_rgba`。
 */
import { screenshotHistoryGet } from "../../ipc/client";
import {
  EXPORT_MIME,
  exportFormatOfMime,
  saveAsName,
  type ExportFormat,
} from "../../windows/overlay/exportFormats";

/** 与 Rust `JPEG_BG_RGB` 同语义：JPEG 无 alpha，这里显式压白底而不是让浏览器丢通道 */
const JPEG_BG = "#ffffff";

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("解码历史图片失败（文件可能已被移动或改坏）"));
    img.src = src;
  });
}

function encodeToBlob(canvas: HTMLCanvasElement, mime: string): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (b) =>
        b
          ? resolve(b)
          : reject(new Error(`${mime} 编码失败（当前环境不支持该输出格式）`)),
      mime,
    );
  });
}

function triggerDownload(href: string, name: string): void {
  const a = document.createElement("a");
  a.href = href;
  a.download = name;
  a.rel = "noopener";
  document.body.append(a);
  a.click();
  a.remove();
}

/**
 * 把一条历史截图另存为指定格式，返回交给浏览器的文件名。
 *
 * 同格式时直接下载原字节：JPEG 套 JPEG 只会二次掉细节，换格式才需要 canvas 重编码。
 */
export async function saveShotAs(
  id: string,
  file: string | null,
  fmt: ExportFormat,
): Promise<string> {
  const data = await screenshotHistoryGet(id);
  const name = saveAsName(file, id, fmt);
  const srcFmt = exportFormatOfMime(data.format);
  if (!srcFmt) throw new Error(`无法识别该历史文件的编码（${data.format || "空"}）`);
  if (srcFmt === fmt) {
    triggerDownload(`data:${EXPORT_MIME[fmt]};base64,${data.png_b64}`, name);
    return name;
  }
  const img = await loadImage(`data:${data.format};base64,${data.png_b64}`);
  const canvas = document.createElement("canvas");
  canvas.width = img.naturalWidth || img.width;
  canvas.height = img.naturalHeight || img.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("无法创建画布上下文，浏览器侧重编码不可用");
  if (fmt === "jpeg") {
    ctx.fillStyle = JPEG_BG;
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  }
  ctx.drawImage(img, 0, 0);
  const blob = await encodeToBlob(canvas, EXPORT_MIME[fmt]);
  const url = URL.createObjectURL(blob);
  try {
    triggerDownload(url, name);
  } finally {
    URL.revokeObjectURL(url);
  }
  return name;
}
