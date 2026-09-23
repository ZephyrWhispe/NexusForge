import type { RemotePresetDto } from "../../ipc/client";

/**
 * 魔术栏目标解析（T-B6-10，09 §6.2）：drive 与 remote 的解析分派**只走这一处**，
 * 面板不得再造第二算式。未知 scheme 不猜——返回 null 由面板提示
 * "无法识别为路径或连接地址"；"当远端主机名试试"式兜底是被禁的那一族假成功。
 */

export type MagicTarget = { kind: "drive"; value: string } | { kind: "remote"; value: string };

/** 前缀 → 协议档：serde 规范名与 URL scheme 惯写名各认一次（webdav/web_dav 皆合法入径） */
const SCHEMES: Record<string, RemotePresetDto["protocol"]> = {
  webdav: "web_dav",
  "http+webdav": "web_dav",
  https: "https",
  sftp: "sftp",
  ftp: "ftp",
};

export function parse_magic_target(
  text: string,
  presets: RemotePresetDto[],
): MagicTarget | null {
  const t = text.trim();
  if (!t) return null;
  // 盘符/UNC 形态：`C:`、`C:\`、`C:/dir` ——与远端主机名无歧义（含冒号且首字符是字母且第二段是路径分隔）
  if (/^[A-Za-z]:([\\/]|$)/.test(t)) return { kind: "drive", value: t };
  const schemeMatch = /^([A-Za-z][A-Za-z0-9+\-.]*):\/\/(.+)$/.exec(t);
  if (schemeMatch) {
    const proto = SCHEMES[schemeMatch[1].toLowerCase()];
    // 认得的 scheme 才走远端；不认得的（如 gopher://）落 null——绝不"当远端主机名试试"
    if (!proto) return null;
    return { kind: "remote", value: `${proto}://${schemeMatch[2]}` };
  }
  // 预设主机直呼：`files.example.com/dir` 命中某档 default_host ⇒ 远端
  for (const p of presets) {
    if (!p.default_host) continue;
    if (t === p.default_host || t.startsWith(`${p.default_host}/`)) {
      return { kind: "remote", value: `${p.protocol}://${t}` };
    }
  }
  return null;
}
