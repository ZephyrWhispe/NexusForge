/** 是否运行在 Tauri WebView 内（浏览器 HMR 预览为 false） */
export const IN_TAURI = "__TAURI_INTERNALS__" in window;
