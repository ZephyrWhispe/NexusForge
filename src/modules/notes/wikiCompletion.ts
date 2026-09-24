/**
 * [[双链]] 补全（T-B7-23）：`[[` 触发 monaco 补全，候选来自 notes_list 标题缓存。
 * 纯函数核（wikiSuggest）与 monaco 适配（registerWikiCompletion）分离——
 * 任务书判据测在核上跑，适配层只持有 API 形状。
 */
import { languageForPath, monaco } from "../../monaco/setup";
import type { NoteMetaDto } from "../../ipc/client";

export type WikiCandidate = { title: string; path: string };
export type WikiSuggestion = {
  insertText: string;
  /** 1 基行列；startColumn 指向第一个 `[`，endColumn=光标列（覆盖 `[[` 前缀与已筛文字） */
  range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number };
  label: string;
};

/** 标题缓存（refreshList 喂；provider 读） */
let titleCache: WikiCandidate[] = [];

export function setWikiTitles(list: NoteMetaDto[]): void {
  titleCache = list.map((n) => ({ title: n.title, path: n.path }));
}

/**
 * 行内光标前文本 → 是否处于未闭合 `[[…` 引用中；是则按已筛文字给候选。
 * 规则：找最后一个 `[[`；其后至光标不得出现 `]]`（视为已闭合）或换行（入参只给行内文本天然无换行）。
 * 筛词大小写不敏感（title/path 任一含之即候选）。insertText 恒为 `[[标题]]` 逐字形。
 */
export function wikiSuggest(
  textUntilCursor: string,
  lineNumber: number,
  cursorColumn: number,
  candidates: WikiCandidate[] = titleCache,
): WikiSuggestion[] | null {
  const start = textUntilCursor.lastIndexOf("[[");
  if (start < 0) return null;
  const partial = textUntilCursor.slice(start + 2);
  if (partial.includes("]]")) return null;
  const filter = partial.trim().toLowerCase();
  const hits = candidates.filter(
    (c) =>
      !filter ||
      c.title.toLowerCase().includes(filter) ||
      c.path.toLowerCase().includes(filter),
  );
  return hits.map((c) => ({
    insertText: `[[${c.title}]]`,
    range: {
      startLineNumber: lineNumber,
      startColumn: start + 1,
      endLineNumber: lineNumber,
      endColumn: cursorColumn,
    },
    label: `${c.title}（${c.path}）`,
  }));
}

let registered = false;

/** 一次性注册 markdown 语言的 `[[` 补全 provider（NotesPanel 挂载时调用） */
export function registerWikiCompletion(): void {
  if (registered) return;
  registered = true;
  monaco.languages.registerCompletionItemProvider(languageForPath("x.md"), {
    triggerCharacters: ["["],
    provideCompletionItems: (model: monaco.editor.ITextModel, position: monaco.Position) => {
      const lineText = model.getValueInRange({
        startLineNumber: position.lineNumber,
        startColumn: 1,
        endLineNumber: position.lineNumber,
        endColumn: position.column,
      });
      const sugg = wikiSuggest(lineText, position.lineNumber, position.column);
      const suggestions = (sugg ?? []).map((s) => ({
        label: s.label,
        kind: monaco.languages.CompletionItemKind.Reference,
        insertText: s.insertText,
        range: s.range,
      }));
      return { suggestions };
    },
  });
}
