import { invoke } from "@tauri-apps/api/core";

/**
 * 类型化 IPC 客户端（docs/UI-PLAN.md U2-2 雏形）。
 * 错误契约（M1 冻结，host-core/src/error.rs）：{ kind, data: { code, message, hint? } }
 */

/** AppError DTO（与 Rust 侧 serde tag/content 序列化一致） */
export interface AppErrorDto {
  kind: "Module" | "Storage" | "Network" | "Permission" | "Config";
  data: { code: string; message: string; hint?: string; retryable?: boolean };
}

/** 把 invoke 抛出的任意值规范化为 AppErrorDto（浏览器/未接 IPC 时返回 null） */
export function parseAppError(e: unknown): AppErrorDto | null {
  if (
    e &&
    typeof e === "object" &&
    "kind" in e &&
    "data" in e &&
    typeof (e as AppErrorDto).data?.code === "string"
  ) {
    return e as AppErrorDto;
  }
  return null;
}

/** 模块状态 DTO（与 src-tauri state.rs ModuleStatusDto 对齐） */
export type ModuleState = "Uninitialized" | "Stopped" | "Running" | "Error";
export interface ModuleStatusDto {
  id: string;
  name: string;
  version: string;
  priority: number;
  state: ModuleState;
}

const IN_TAURI = "__TAURI_INTERNALS__" in window;

/** 读取 Windows 系统强调色（hex）；不可用返回 null */
export async function hostSystemAccent(): Promise<string | null> {
  try {
    return await invoke<string>("host_system_accent");
  } catch {
    return null;
  }
}

/** 全部模块状态；浏览器预览返回 null（UI 走静态默认） */
export async function hostModulesStatus(): Promise<ModuleStatusDto[] | null> {
  if (!IN_TAURI) return null;
  try {
    return await invoke<ModuleStatusDto[]>("host_modules_status");
  } catch {
    return null;
  }
}

/** 重启模块（Error 态恢复入口，docs/impl/01 S5） */
export async function hostModuleRestart(id: string): Promise<void> {
  await invoke("host_module_restart", { id });
}

/** 读模块配置 */
export async function hostConfigGet<T = Record<string, unknown>>(module: string): Promise<T> {
  return invoke<T>("host_config_get", { module });
}

/** 写模块配置（Rust 侧 schema 校验失败将抛 AppErrorDto） */
export async function hostConfigSet(module: string, values: unknown): Promise<void> {
  await invoke("host_config_set", { module, values });
}

// ---------------- 剪切板 IPC（docs/impl/02 C7 DTO 对齐）----------------

export interface ClipEntry {
  id: string;
  content_type: "text" | "files" | "image";
  preview: string;
  blob_path: string | null;
  origin: "local" | "remote";
  source_app: string | null;
  pinned: boolean;
  group: string | null;
  secret: boolean;
  created_at: number;
  usage_count: number;
}

export interface ClipPage {
  items: ClipEntry[];
  has_more: boolean;
  total: number | null;
}

export interface ClipSearchQuery {
  text?: string;
  group?: string;
  page?: number;
  size?: number;
}

export function clipboardSearch(query: ClipSearchQuery): Promise<ClipPage> {
  return invoke<ClipPage>("clipboard_search", { query });
}
export function clipboardGet(id: string): Promise<string> {
  return invoke<string>("clipboard_get", { id });
}
export function clipboardPaste(id: string): Promise<void> {
  return invoke("clipboard_paste", { id });
}
export function clipboardPin(id: string, pinned: boolean): Promise<void> {
  return invoke("clipboard_pin", { id, pinned });
}
export function clipboardDelete(id: string): Promise<void> {
  return invoke("clipboard_delete", { id });
}
export function clipboardClear(keepPinned: boolean): Promise<number> {
  return invoke("clipboard_clear", { keepPinned });
}
/** 分组计数（SubNav 角标） */
export function clipboardGroupCounts(): Promise<Record<string, number>> {
  return invoke("clipboard_group_counts");
}
/** 图片条目字节（Base64 DIB） */
export function clipboardGetImage(id: string): Promise<string> {
  return invoke<string>("clipboard_get_image", { id });
}
/** 读模块配置 schema（设置中心自动渲染） */
export function hostConfigSchema(module: string): Promise<Record<string, unknown>> {
  return invoke("host_config_schema", { module });
}
