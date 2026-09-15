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
