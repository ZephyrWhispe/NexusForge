/**
 * 导出格式词表（09 §9.2 T-B4-7 前端侧）。
 *
 * 三张表与 Rust `util::EncodeFormat` 的三个方法逐键对齐（`ext` / `content_type` /
 * 变体名）：磁盘上那个名字与浏览器里那个 MIME 都由这一处给出，两处各写一遍必然漂出
 * `shot.jpeg` 或 `data:image/jpg` 这类"看着对、实际错"的值。
 */
import type { AnnotationDto, FinishRequestDto } from "../../ipc/client";

export const EXPORT_FORMATS = ["png", "jpeg", "webp"] as const;
export type ExportFormat = (typeof EXPORT_FORMATS)[number];

/** 磁盘扩展名（注意 jpeg → jpg：与 Rust `EncodeFormat::ext` 同形） */
export const EXPORT_EXT: Record<ExportFormat, string> = {
  png: "png",
  jpeg: "jpg",
  webp: "webp",
};

export const EXPORT_MIME: Record<ExportFormat, string> = {
  png: "image/png",
  jpeg: "image/jpeg",
  webp: "image/webp",
};

/** 界面标签（覆盖层格式钮与历史"另存为"菜单同源，两处各写会分叉出两种叫法） */
export const EXPORT_LABEL: Record<ExportFormat, string> = {
  png: "PNG",
  jpeg: "JPEG",
  webp: "WebP",
};

export function isExportFormat(v: string | null | undefined): v is ExportFormat {
  return v !== null && v !== undefined && (EXPORT_FORMATS as readonly string[]).includes(v);
}

/**
 * 覆盖层"存为"钮的循环序：null（跟随设置）→ png → jpeg → webp → png …
 * 首跳刻意落在 png 而不是回到 null：再点一次就回默认，用户永远出不去当前选择；
 * 而"未选过"这一态由 null 表达，所以刚打开覆盖层时钮上写的是"跟随设置"。
 */
export function nextExportFormat(cur: ExportFormat | null): ExportFormat {
  if (!cur) return EXPORT_FORMATS[0];
  const i = EXPORT_FORMATS.indexOf(cur);
  return EXPORT_FORMATS[(i + 1) % EXPORT_FORMATS.length];
}

/** 反向查表：`history_get` 回传的嗅探 MIME → 可选格式（查不到就是这文件不是本模块写的） */
export function exportFormatOfMime(mime: string): ExportFormat | null {
  return (EXPORT_FORMATS as readonly ExportFormat[]).find((f) => EXPORT_MIME[f] === mime) ?? null;
}

export interface FinishRequestInput {
  image_b64: string;
  actions: string[];
  annotations: AnnotationDto[];
  /** null = 跟随设置里的 `format` */
  format: ExportFormat | null;
  pin_x?: number | null;
  pin_y?: number | null;
}

/**
 * 组装 `screenshot_finish` 请求体。放在纯模块而不是调用点：`format` 缺席（=用配置默认）
 * 与 `format: "jpeg"`（=本次覆盖）是两种语义，"不传这个键"用对象字面量表达时最容易被
 * 顺手写成 `format: null`——那样 Rust 侧收到的就是显式的坏值而不是默认。
 */
export function finishRequestBody(input: FinishRequestInput): FinishRequestDto {
  return {
    image_b64: input.image_b64,
    actions: input.actions,
    pin_x: input.pin_x ?? null,
    pin_y: input.pin_y ?? null,
    annotations: input.annotations,
    ...(input.format ? { format: input.format } : {}),
  };
}

/**
 * 另存为的文件名：沿用历史行的主名、只换扩展名（`x.png` + jpeg → `x.jpg`，
 * 不是 `x.png.jpg`）；无文件名（未保存过的记录）时退回 `shot_{id 前缀}`。
 */
export function saveAsName(file: string | null, id: string, fmt: ExportFormat): string {
  const base = file ? (file.split(/[/\\]/).pop() ?? "") : "";
  const stem = base.replace(/\.[^./\\]+$/, "") || `shot_${id.slice(0, 8)}`;
  return `${stem}.${EXPORT_EXT[fmt]}`;
}
