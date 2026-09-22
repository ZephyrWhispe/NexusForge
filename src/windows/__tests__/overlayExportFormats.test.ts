/**
 * 覆盖层"另存为"格式钮（09 §9.2 T-B4-7）：格式词表 + 请求体组装 + 文件名换后缀。
 *
 * §9.1-④ 两半制沿用：语义住纯模块（本文件直接调用），装配面（OverlayShot 装载不了
 * jsdom——它要真 canvas 2d 上下文与 Tauri 窗口 API）以 `?raw` 源码扫描钉住调用形状。
 */
import { describe, expect, it } from "vitest";
import overlaySrc from "../OverlayShot.tsx?raw";
import {
  EXPORT_EXT,
  EXPORT_FORMATS,
  EXPORT_LABEL,
  EXPORT_MIME,
  exportFormatOfMime,
  finishRequestBody,
  isExportFormat,
  nextExportFormat,
  saveAsName,
} from "../overlay/exportFormats";

describe("导出格式词表与请求组装（T-B4-7）", () => {
  it("overlaySaveAs_formatChip_reachesFinishRequest", () => {
    // 模型半：选了 jpeg → 请求体里就得有 format: "jpeg"（字面判据）
    expect(
      finishRequestBody({
        image_b64: "QUJD",
        actions: ["save"],
        annotations: [],
        format: "jpeg",
      }),
    ).toEqual(expect.objectContaining({ format: "jpeg" }));
    // 负例（=正对照的另一面）：null 表示"跟随设置"，这一键必须整个不出现。
    // 写成 format: null 会被宿主当成一次显式选择并在解析点报错。
    const follow = finishRequestBody({
      image_b64: "QUJD",
      actions: [],
      annotations: [],
      format: null,
    });
    expect("format" in follow).toBe(false);
    expect(follow.image_b64).toBe("QUJD"); // 其余键照常在场（防空洞：不是返回了空对象）
    expect(follow.pin_x).toBeNull();
    // 装配半：钮按循环序翻格式，标签与请求体都读同一个 state
    expect(overlaySrc).toContain("onClick={() => setSaveFormat(nextExportFormat(saveFormat))}");
    expect(overlaySrc).toContain("存为 {saveFormat ? EXPORT_LABEL[saveFormat] : \"跟随设置\"}");
    expect(overlaySrc).toContain("finishRequestBody({");
    expect(overlaySrc).toContain("format: saveFormat,");
    // 装配面不许自己造第三条路（把"跟随设置"写成 format: null 会被宿主当成显式坏值）
    expect(overlaySrc).not.toContain("format: null");
  });

  it("词表三张同键：ext / mime / label 一一对齐 png·jpeg·webp", () => {
    expect(EXPORT_FORMATS).toEqual(["png", "jpeg", "webp"]);
    for (const f of EXPORT_FORMATS) {
      expect(EXPORT_MIME[f].startsWith("image/")).toBe(true);
      expect(EXPORT_LABEL[f]).toBeTruthy();
      // 扩展名词表与 Rust EncodeFormat::ext 同形：jpeg 落 .jpg（不是 .jpeg）
      expect(EXPORT_EXT[f]).toBe(f === "jpeg" ? "jpg" : f);
      // 反向查表闭合（面板拿 MIME、钮拿格式，两处必须能互相走通）
      expect(exportFormatOfMime(EXPORT_MIME[f])).toBe(f);
    }
    expect(exportFormatOfMime("image/gif")).toBeNull();
    expect(exportFormatOfMime("unknown")).toBeNull();
    expect(exportFormatOfMime("")).toBeNull();
    expect(isExportFormat("png")).toBe(true);
    expect(isExportFormat("tiff")).toBe(false);
    expect(isExportFormat(undefined)).toBe(false);
  });

  it("nextExportFormat 循环：未选→png→jpeg→webp→png（不回 null，用户出得去当前选择）", () => {
    expect(nextExportFormat(null)).toBe("png");
    expect(nextExportFormat("png")).toBe("jpeg");
    expect(nextExportFormat("jpeg")).toBe("webp");
    expect(nextExportFormat("webp")).toBe("png");
  });

  it("saveAsName 换后缀而不叠后缀", () => {
    expect(saveAsName("C:\\Users\\me\\shots\\shot_1.png", "id1", "jpeg")).toBe("shot_1.jpg");
    expect(saveAsName("/x/y/shot_1.png", "id1", "webp")).toBe("shot_1.webp");
    // 同格式请求：名字原样（png 文件另存为 png 不改名）
    expect(saveAsName("/x/y/shot_1.png", "id1", "png")).toBe("shot_1.png");
    // 无文件名（未保存过的记录）退回 id 前缀，而不是拼出 ".jpg" 这种无名文件
    expect(saveAsName(null, "0192abcd-ef56-7890", "jpeg")).toBe("shot_0192abcd.jpg");
    expect(saveAsName("", "0192abcd-ef56-7890", "png")).toBe("shot_0192abcd.png");
    // 无扩展名的主名保持不动（正则不吃没有点的名字）
    expect(saveAsName("/x/dup", "id1", "webp")).toBe("dup.webp");
  });
});
