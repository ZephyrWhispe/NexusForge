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

/** 前端日志上报进宿主日志（webview console 外部不可见） */
export function hostLog(level: "info" | "warn" | "error", message: string): void {
  invoke("host_log", { level, message }).catch(() => undefined);
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

// ---------------- 截图 IPC（docs/impl/03 P8 DTO 对齐）----------------

export interface TaskStartDto {
  task_id: string;
  /** 虚拟桌面原点与尺寸（物理像素），覆盖层窗口定位用 */
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface TaskInfoDto {
  task_id: string;
  /** shot | ocr */
  mode: string;
  width: number;
  height: number;
  png_b64: string;
}

export interface ConfirmRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface CropDto {
  png_b64: string;
  width: number;
  height: number;
}

export interface AnnotationDto {
  kind: "pen" | "rect" | "ellipse" | "arrow" | "text" | "mosaic" | "number";
  color: string;
  width: number;
  points: [number, number][];
  text?: string | null;
  seq?: number | null;
}

export interface FinishRequestDto {
  image_b64: string;
  /** save | copy | pin；空 = 应用设置中的默认动作 */
  actions: string[];
  pin_x?: number | null;
  pin_y?: number | null;
  annotations?: AnnotationDto[];
}

export interface FinishDto {
  file: string | null;
  pin_id: string | null;
}

export interface ShotItemDto {
  id: string;
  created_ms: number;
  width: number;
  height: number;
  file: string | null;
  ocr_text: string | null;
}

export interface ShotPageDto {
  items: ShotItemDto[];
  total: number;
  page: number;
  size: number;
}

export interface PinDto {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
  zoom: number;
  opacity: number;
}

export interface PinDataDto extends PinDto {
  png_b64: string;
}

/** 启动截图（抓全屏帧，返回覆盖层定位） */
export function screenshotStart(mode: "shot" | "ocr"): Promise<TaskStartDto> {
  return invoke("screenshot_start", { mode });
}
/** 覆盖层取背景帧 */
export function screenshotTask(taskId: string): Promise<TaskInfoDto> {
  return invoke("screenshot_task", { taskId });
}
/** 选区确认（物理像素，帧相对坐标） */
export function screenshotConfirm(taskId: string, rect: ConfirmRect): Promise<CropDto> {
  return invoke("screenshot_confirm", { taskId, rect });
}
/** 丢弃任务（取消时释放帧内存） */
export function screenshotDiscard(taskId: string): Promise<void> {
  return invoke("screenshot_discard", { taskId });
}
/** 完成（合成图 + 动作） */
export function screenshotFinish(taskId: string, request: FinishRequestDto): Promise<FinishDto> {
  return invoke("screenshot_finish", { taskId, request });
}
/** 截图历史分页 */
export function screenshotHistoryList(page: number, size: number): Promise<ShotPageDto> {
  return invoke("screenshot_history_list", { query: { page, size } });
}
/** 全部贴图（启动恢复） */
export function screenshotPins(): Promise<PinDto[]> {
  return invoke("screenshot_pins");
}
/** 贴图数据 */
export function screenshotPinGet(id: string): Promise<PinDataDto> {
  return invoke("screenshot_pin_get", { id });
}
/** 贴图缩放/透明度持久化 */
export function screenshotPinUpdate(id: string, zoom: number, opacity: number): Promise<void> {
  return invoke("screenshot_pin_update", { id, zoom, opacity });
}
/** 关闭贴图 */
export function screenshotPinClose(id: string): Promise<void> {
  return invoke("screenshot_pin_close", { id });
}

// ---------------- OCR IPC（docs/impl/04 O7 DTO 对齐）----------------

export interface OcrRequestDto {
  image_b64: string;
  /** 偏好语言（BCP-47），空 = 系统默认 */
  langs?: string[];
  source_task_id?: string | null;
}

export interface OcrLineDto {
  text: string;
  rect: { x: number; y: number; w: number; h: number };
  confidence: number;
}

export interface OcrResultDto {
  lines: OcrLineDto[];
  text: string;
  lang: string;
  engine: string;
}

export interface EngineStatusDto {
  engines: { id: string; name: string; available: boolean }[];
  languages: string[];
}

/** 识别 */
export function ocrRecognize(request: OcrRequestDto): Promise<OcrResultDto> {
  return invoke("ocr_recognize", { request });
}
/** 引擎状态 */
export function ocrEngineStatus(): Promise<EngineStatusDto> {
  return invoke("ocr_engine_status");
}
/** OCR 文本复制到剪贴板（进入剪贴板历史） */
export function ocrCopyText(text: string): Promise<void> {
  return invoke("ocr_copy_text", { text });
}

// ---------------- KVM 键鼠共享 IPC（docs/impl/05 K8）----------------

/** 已配对设备 */
export interface PairedPeerDto {
  device_id: string;
  device_name: string;
  fingerprint: string;
  pubkey_b64: string;
  paired_at: number;
}

/** 心跳发现的邻居 */
export interface PeerInfoDto {
  device_id: string;
  device_name: string;
  pubkey_fingerprint: string;
  tcp_port: number;
  caps: string[];
  addr: string;
  screen: { x: number; y: number; w: number; h: number };
}

/** 活跃会话（role: client=本端发起 / server=对端接入） */
export interface SessionDto {
  device_id: string;
  device_name: string;
  role: "client" | "server";
}

/** 控制状态（idle/controlling/controlled） */
export interface ControlStateDto {
  role: "idle" | "controlling" | "controlled";
  device_id?: string;
}

/** 签发一次性配对码（返回 [码, 有效期毫秒]） */
export function kvmIssuePairCode(): Promise<[string, number]> {
  return invoke("kvm_issue_pair_code");
}
/** 向已发现设备发起配对（对端在其 UI 输入本端码，或反之） */
export function kvmPairWith(addr: string, code: string): Promise<PairedPeerDto> {
  return invoke("kvm_pair_with", { addr, code });
}
/** 解除配对 */
export function kvmUnpair(deviceId: string): Promise<boolean> {
  return invoke("kvm_unpair", { deviceId });
}
/** 已配对设备列表 */
export function kvmPairedPeers(): Promise<PairedPeerDto[]> {
  return invoke("kvm_paired_peers");
}
/** 已发现邻居列表 */
export function kvmDiscoveredPeers(): Promise<PeerInfoDto[]> {
  return invoke("kvm_discovered_peers");
}
/** 向已配对设备发起会话（返回对端 device_id） */
export function kvmConnectTo(addr: string): Promise<string> {
  return invoke("kvm_connect_to", { addr });
}
/** 发送剪贴板内容（Text/Image） */
export function kvmSendClip(
  deviceId: string,
  content: { Text?: { text: string; html?: string | null } } | { Image?: unknown },
): Promise<void> {
  return invoke("kvm_send_clip", { deviceId, content });
}
/** 发送本地文件 */
export function kvmSendFile(deviceId: string, path: string): Promise<void> {
  return invoke("kvm_send_file", { deviceId, path });
}
/** 活跃会话列表 */
export function kvmSessionList(): Promise<SessionDto[]> {
  return invoke("kvm_session_list");
}
/** 设置 [设备→共享边] 映射 */
export function kvmSetEdgeMap(map: Record<string, string>): Promise<void> {
  return invoke("kvm_set_edge_map", { map });
}
/** 当前边缘映射 */
export function kvmEdgeMap(): Promise<Record<string, string>> {
  return invoke("kvm_edge_map");
}
/** 控制状态 */
export function kvmControlState(): Promise<ControlStateDto> {
  return invoke("kvm_control_state");
}
/** 手动释放控制权 */
export function kvmReleaseControl(): Promise<void> {
  return invoke("kvm_release_control");
}

// ---------------- 密码库 IPC（docs/impl/05 V7 DTO 对齐）----------------

/** 密码库状态（三态：uninitialized / locked / unlocked） */
export interface VaultStatusDto {
  state: "uninitialized" | "locked" | "unlocked";
  lockout_remaining_secs: number;
  /** 头部快照（KDF 参数 / vault_id，无机密） */
  kdf: {
    version: number;
    vault_id: string;
    kdf: { algo: string; m_cost_kib: number; t_cost: number; p_cost: number; salt_b64: string };
    wrapped_dek: { nonce_b64: string; ct_b64: string };
  } | null;
}

export type FieldKindDto = "password" | "url" | "note" | "otp" | "text";

export interface EntryFieldDto {
  key: string;
  kind: FieldKindDto;
  value: string;
}

export interface VaultEntryDto {
  id: string;
  folder_id: string | null;
  title: string;
  favorite: boolean;
  fields: EntryFieldDto[];
  totp_secret: string | null;
  created_at: number;
  updated_at: number;
}

export interface VaultFolderDto {
  id: string;
  name: string;
  created_at: number;
}

export interface PasswordPolicyDto {
  length: number;
  upper: boolean;
  lower: boolean;
  digits: boolean;
  symbols: boolean;
  avoid_ambiguous: boolean;
}

export function vaultStatus(): Promise<VaultStatusDto> {
  return invoke("vault_status");
}
export function vaultCreate(masterPassword: string): Promise<VaultStatusDto["kdf"]> {
  return invoke("vault_create", { masterPassword });
}
export function vaultUnlock(masterPassword: string): Promise<void> {
  return invoke("vault_unlock", { masterPassword });
}
export function vaultLock(): Promise<void> {
  return invoke("vault_lock");
}
export function vaultChangeMasterPassword(oldPassword: string, newPassword: string): Promise<unknown> {
  return invoke("vault_change_master_password", { oldPassword, newPassword });
}
export function vaultFolders(): Promise<VaultFolderDto[]> {
  return invoke("vault_folders");
}
export function vaultFolderCreate(name: string): Promise<VaultFolderDto> {
  return invoke("vault_folder_create", { name });
}
export function vaultFolderRename(id: string, name: string): Promise<boolean> {
  return invoke("vault_folder_rename", { id, name });
}
export function vaultFolderDelete(id: string): Promise<boolean> {
  return invoke("vault_folder_delete", { id });
}
export function vaultEntries(folderId: string | null, search: string | null): Promise<VaultEntryDto[]> {
  return invoke("vault_entries", { folderId, search });
}
export function vaultEntryGet(id: string): Promise<VaultEntryDto | null> {
  return invoke("vault_entry_get", { id });
}
export function vaultEntryAdd(input: {
  title: string;
  folder_id: string | null;
  favorite: boolean;
  fields: EntryFieldDto[];
  totp_secret: string | null;
}): Promise<VaultEntryDto> {
  return invoke("vault_entry_add", input);
}
export function vaultEntryUpdate(entry: VaultEntryDto): Promise<VaultEntryDto> {
  return invoke("vault_entry_update", { entry });
}
export function vaultEntryDelete(id: string): Promise<boolean> {
  return invoke("vault_entry_delete", { id });
}
export function vaultGeneratePassword(policy: PasswordPolicyDto): Promise<string> {
  return invoke("vault_generate_password", { policy });
}
/** 当前 TOTP 码 + 剩余秒数 */
export function vaultTotpNow(secret: string): Promise<[string, number]> {
  return invoke("vault_totp_now", { secret });
}
