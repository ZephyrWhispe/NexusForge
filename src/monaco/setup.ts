import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
import * as monaco from "monaco-editor";

/**
 * Monaco 本地集成（docs/impl/06 E2）：
 * - 全部经 npm 本地打包（Vite ?worker），离线可用，CSP null 下 worker 正常运行
 * - 大文件降级在 EditorPanel 控制 model 语言为 plaintext + 关 minimap
 */
self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string): Worker {
    if (label === "json") return new jsonWorker();
    if (label === "css" || label === "scss" || label === "less") return new cssWorker();
    if (label === "html" || label === "handlebars" || label === "razor") return new htmlWorker();
    if (label === "typescript" || label === "javascript") return new tsWorker();
    return new editorWorker();
  },
};

/** Monaco 语言推断（按扩展名；v1 覆盖常见文本类型） */
export function languageForPath(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript",
    json: "json", md: "markdown", html: "html", htm: "html", css: "css",
    scss: "scss", less: "less", py: "python", rs: "rust", go: "go",
    java: "java", c: "c", h: "c", cpp: "cpp", hpp: "cpp", cs: "csharp",
    xml: "xml", yml: "yaml", yaml: "yaml", toml: "ini", ini: "ini",
    sql: "sql", sh: "shell", bat: "bat", ps1: "powershell", lua: "lua",
  };
  return map[ext] ?? "plaintext";
}

export { monaco };
