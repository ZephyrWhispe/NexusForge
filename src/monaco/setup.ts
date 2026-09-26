import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
// R-I4（D-37·PERF-03 半项）：全量入口 `monaco-editor`（＝editor.main，捆绑 ~90 门
// basic-languages）改按需装配——能力面按 languageForPath 实际服务的语言集合取，
// 编辑器特性（查找/折叠/建议等）由 editor.all 全保。
import "monaco-editor/esm/vs/editor/editor.all";
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";

// 语言服务四门（真机冒烟坐实：服务贡献只挂 onLanguage 补全/校验钩子，
// 语言 ID 与 monarch 着色器一律由 basic-languages 注册——见下方六门补登）
import "monaco-editor/esm/vs/language/typescript/monaco.contribution";
import "monaco-editor/esm/vs/language/json/monaco.contribution";
import "monaco-editor/esm/vs/language/css/monaco.contribution";
import "monaco-editor/esm/vs/language/html/monaco.contribution";
// 语言服务四门的语言 ID 注册面（json 的 ID 由其服务贡献自带，故不在此列）
import "monaco-editor/esm/vs/basic-languages/typescript/typescript.contribution";
import "monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution";
import "monaco-editor/esm/vs/basic-languages/css/css.contribution";
import "monaco-editor/esm/vs/basic-languages/scss/scss.contribution";
import "monaco-editor/esm/vs/basic-languages/less/less.contribution";
import "monaco-editor/esm/vs/basic-languages/html/html.contribution";
// 其余 languageForPath 服务的语言（cpp 包连带注册 c/cpp；纯词法着色无语言服务）
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution";
import "monaco-editor/esm/vs/basic-languages/python/python.contribution";
import "monaco-editor/esm/vs/basic-languages/rust/rust.contribution";
import "monaco-editor/esm/vs/basic-languages/go/go.contribution";
import "monaco-editor/esm/vs/basic-languages/java/java.contribution";
import "monaco-editor/esm/vs/basic-languages/cpp/cpp.contribution";
import "monaco-editor/esm/vs/basic-languages/csharp/csharp.contribution";
import "monaco-editor/esm/vs/basic-languages/xml/xml.contribution";
import "monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution";
import "monaco-editor/esm/vs/basic-languages/ini/ini.contribution";
import "monaco-editor/esm/vs/basic-languages/sql/sql.contribution";
import "monaco-editor/esm/vs/basic-languages/shell/shell.contribution";
import "monaco-editor/esm/vs/basic-languages/bat/bat.contribution";
import "monaco-editor/esm/vs/basic-languages/powershell/powershell.contribution";
import "monaco-editor/esm/vs/basic-languages/lua/lua.contribution";

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
