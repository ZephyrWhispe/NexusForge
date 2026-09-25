// SEC-07 回归：Markdown 预览必须净化脚本/事件处理器/危险协议（marked@15 无
// sanitize 且对原始 HTML 透传；笔记内容经 sync 跨设备同步 = 远端不可信）。

import { describe, expect, it } from "vitest";
import { renderMarkdown } from "../MarkdownView";

describe("renderMarkdown 净化", () => {
  it("剥离脚本标签与事件处理器", () => {
    const html = renderMarkdown(
      '<img src=x onerror="alert(1)"><script>alert(2)</script><a href="javascript:alert(3)">x</a>',
    );
    expect(html).not.toMatch(/onerror/i);
    expect(html).not.toMatch(/<script/i);
    expect(html).not.toMatch(/javascript:/i);
  });

  it("剥离 style/iframe/base/内联事件属性", () => {
    const html = renderMarkdown(
      '<style>body{display:none}</style><iframe src="https://evil"></iframe>' +
        '<base href="https://evil/"><div onclick="alert(1)">t</div>' +
        '<img src="https://ok/x.png" style="position:fixed">',
    );
    expect(html).not.toMatch(/<style/i);
    expect(html).not.toMatch(/<iframe/i);
    expect(html).not.toMatch(/<base/i);
    expect(html).not.toMatch(/onclick/i);
    expect(html).not.toMatch(/style=/i);
  });

  it("仅放行安全协议链接与图片", () => {
    const ok = renderMarkdown("[a](https://example.com) 与 ![b](data:image/png;base64,aGk=)");
    expect(ok).toContain('href="https://example.com"');
    expect(ok).toMatch(/src="data:image\/png;base64,/);
    const bad = renderMarkdown('[x](file:///C:/Windows) 与 [y](vbscript:msgbox)');
    expect(bad).not.toMatch(/file:/i);
    expect(bad).not.toMatch(/vbscript:/i);
  });

  it("常规 Markdown 语法不受净化影响", () => {
    const html = renderMarkdown("# 标题\n\n**粗体** 与 `code` 与 [[双链]]\n");
    expect(html).toContain("<h1>");
    expect(html).toContain("<strong>");
    expect(html).toContain("<code>");
  });
});
