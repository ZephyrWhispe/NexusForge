//! Markdown 共享渲染组件（SEC-07）：`marked.parse()` 输出**必须**经 DOMPurify
//! 净化后才可进入 DOM。marked@15 无 sanitize 选项且对原始 HTML 透传——笔记
//! 内容经 sync 跨设备同步（远端不可信），未净化即等同跨设备 XSS → IPC 越权。
//!
//! 纪律：全仓 `dangerouslySetInnerHTML` 仅允许出现在本组件（security_config
//! 断言 + ESLint 门禁）；新渲染点一律复用 `renderMarkdown` / `MarkdownView`。

import { useMemo } from "react";
import DOMPurify from "dompurify";
import { marked } from "marked";

/** 白名单标签（input 仅供 task-list 勾选框） */
const ALLOWED_TAGS = [
  "h1", "h2", "h3", "h4", "h5", "h6",
  "p", "br", "hr",
  "ul", "ol", "li", "blockquote",
  "pre", "code", "strong", "em", "del", "ins",
  "table", "thead", "tbody", "tr", "th", "td",
  "a", "img", "input", "span",
];

/** 白名单属性（不含任何 on* 事件属性与 style） */
const ALLOWED_ATTR = ["href", "title", "alt", "src", "class", "type", "checked", "disabled", "align"];

/** 仅允许安全协议的链接与图片（data: 限图片 base64） */
const ALLOWED_URI_REGEXP = /^(?:https?|mailto|data:image\/(?:png|jpe?g|gif|webp);base64,)/i;

/** 解析 + 净化（唯一出口；调用方不得绕过本函数直用 marked） */
export function renderMarkdown(src: string): string {
  const raw = marked.parse(src, { async: false }) as string;
  return DOMPurify.sanitize(raw, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    ALLOWED_URI_REGEXP,
    FORBID_TAGS: ["style", "script", "iframe", "object", "embed", "form", "base", "link"],
    FORBID_ATTR: ["style", "formaction", "xlink:href"],
  });
}

export default function MarkdownView({
  source,
  className,
  divRef,
}: {
  source: string;
  className?: string;
  /** 外部需要滚动同步/定位时透传（ref 仍指向净化后的同一容器） */
  divRef?: React.Ref<HTMLDivElement>;
}) {
  const html = useMemo(() => renderMarkdown(source), [source]);
  return <div ref={divRef} className={className} dangerouslySetInnerHTML={{ __html: html }} />;
}
