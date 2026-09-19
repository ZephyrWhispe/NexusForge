import { describe, expect, it } from "vitest";
// vite `?raw` 导入：断言对象就是构建器眼见的源文件本身。
// （global.css 基底规则的回归断言在 src-tauri/tests/security_config.rs——vitest 会 stub CSS 模块，
// ?raw 也拿不到文本，改由 Rust 侧用真实文件系统判定。）
import indexHtml from "../../index.html?raw";

// 打包期 Tauri 会给 index.html 里的静态 <style> 注入 nonce，使 CSP 的 style-src 变成
// `'unsafe-inline' 'nonce-…'`；而 CSP 规范下 directive 一旦出现 nonce/hash，'unsafe-inline'
// 即被浏览器忽略，从而拒掉 Fluent UI(griffel) 运行时注入的、不带该 nonce 的样式 → 整站无样式。
// 因此 index.html 必须不含任何内联 <style>；页面基底透明等全局规则改放链接 CSS（global.css）。
// 参见 docs/DECISIONS.md D-28 及发布后修复记录（安装包 UI 全乱事故）。

/** index.html 中内联 <style> 块的数量（纯函数，自带负例证明判定非恒真）。 */
function inlineStyleCount(html: string): number {
  return (html.match(/<style[\s>]/gi) ?? []).length;
}

describe("index.html 内联样式约束（D-28 发布后 UI 修复回归）", () => {
  it("检测函数本身能命中内联 style（证明断言非恒真）", () => {
    expect(inlineStyleCount("<style>html{}</style>")).toBe(1);
    expect(inlineStyleCount('<style type="text/css">a{}</style><style>b{}</style>')).toBe(2);
    expect(inlineStyleCount("<head><link rel=stylesheet href=x.css></head>")).toBe(0);
    // '<styles>' 等非标签前缀不得误计
    expect(inlineStyleCount("<stylesheet></stylesheet>")).toBe(0);
  });

  it("index.html 不得含内联 <style>（否则触发打包期 style-src nonce 注入）", () => {
    expect(inlineStyleCount(indexHtml)).toBe(0);
  });
});
