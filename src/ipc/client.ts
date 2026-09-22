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
  /** 该行另有 HTML 正文（T-B3-8）：正文不经列表下发，须走 clipboard_html_get 显式取 */
  has_html: boolean;
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
  /** 内容类型显式筛选（T-B3-6 芯片行）；与 text 里的 `type:` 语法同源，后端语法优先 */
  content_type?: string;
}

export function clipboardSearch(query: ClipSearchQuery): Promise<ClipPage> {
  return invoke<ClipPage>("clipboard_search", { query });
}
export function clipboardGet(id: string): Promise<string> {
  return invoke<string>("clipboard_get", { id });
}
/** 格式粘贴回执（T-B3-8）：degraded=true 表示要的是 HTML、实际只投出去了纯文本 */
export interface ClipPasteResult {
  format_used: "plain" | "html";
  degraded: boolean;
}

export function clipboardPaste(id: string, format?: "plain" | "html"): Promise<ClipPasteResult> {
  return invoke<ClipPasteResult>("clipboard_paste", { id, format });
}
/** HTML 源文按需读取（列表只带 has_html 布尔）；无正文即 CLIPBOARD_HTML_001 反错 */
export function clipboardHtmlGet(id: string): Promise<string> {
  return invoke<string>("clipboard_html_get", { id });
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

/** 捕获暂停态（§8-④）：paused 读运行期原子位，skipped 为暂停期间累计跳过次数 */
export interface ClipCaptureState {
  paused: boolean;
  skipped: number;
}

/** 读捕获暂停态（运行期真值，非盘上投影） */
export function clipboardCaptureGet(): Promise<ClipCaptureState> {
  return invoke<ClipCaptureState>("clipboard_capture_get");
}

/** 写捕获暂停态：返回切换后的运行期真值 */
export function clipboardCaptureSet(paused: boolean): Promise<ClipCaptureState> {
  return invoke<ClipCaptureState>("clipboard_capture_set", { paused });
}

// ---------------- 粘贴堆栈（docs/impl/09 §8.2 T-B3-3）----------------

/** 队首投递回执：敏感项 delivered=false 且仍留在栈上（error 点名原因） */
export interface ClipStackPaste {
  id: string;
  delivered: boolean;
  error: string | null;
}

/** 全部粘贴汇总：remaining 为结束后仍在栈上的条数 */
export interface ClipStackReport {
  delivered: number;
  failed: number;
  remaining: number;
}

/** 入栈（幂等），返回入栈后的栈深 */
export function clipboardStackPush(id: string): Promise<number> {
  return invoke<number>("clipboard_stack_push", { id });
}

/** 队列视图（入栈序，已删条目静默少一行） */
export function clipboardStackList(): Promise<ClipEntry[]> {
  return invoke<ClipEntry[]>("clipboard_stack_list");
}

/** 移到目标下标（0 基，越界钳到队尾） */
export function clipboardStackMove(id: string, to: number): Promise<void> {
  return invoke("clipboard_stack_move", { id, to });
}

/** 移出堆栈（历史记录保留）；返回栈上确有其项 */
export function clipboardStackRemove(id: string): Promise<boolean> {
  return invoke<boolean>("clipboard_stack_remove", { id });
}

export function clipboardStackClear(): Promise<number> {
  return invoke<number>("clipboard_stack_clear");
}

/** 粘贴下一条：写剪贴板 + 注入 Ctrl+V；空栈返回 null（不假成功） */
export function clipboardStackPasteNext(): Promise<ClipStackPaste | null> {
  return invoke<ClipStackPaste | null>("clipboard_stack_paste_next");
}

export function clipboardStackPasteAll(intervalMs: number): Promise<ClipStackReport> {
  return invoke<ClipStackReport>("clipboard_stack_paste_all", { intervalMs });
}

// ---------------- 分组数据面 + 智能建议 + 统计（docs/impl/09 §8.2 T-B3-4）----------------

/** 待采纳的分组建议：preview 已遮蔽敏感项，confidence 为分类器原值 */
export interface ClipSuggestion {
  entry_id: string;
  preview: string;
  suggested_group: string;
  confidence: number;
}

/** 统计卡：by_content_type / by_group 为「名 → 条数」映射，未分组桶名为 "未分组" */
export interface ClipStats {
  total: number;
  by_content_type: Record<string, number>;
  by_group: Record<string, number>;
  top_source_apps: [string, number][];
  bytes_blob: number;
}

/** 改单条目分组：group=null 表示取消分组 */
export function clipboardEntrySetGroup(id: string, group: string | null): Promise<void> {
  return invoke("clipboard_entry_set_group", { id, group });
}

/** 整组重命名，返回影响行数（组名可为任意 Unicode，含空格与逗号） */
export function clipboardGroupRename(from: string, to: string): Promise<number> {
  return invoke<number>("clipboard_group_rename", { from, to });
}

/** 删除分组：组内条目回到未分组，历史记录保留 */
export function clipboardGroupDelete(name: string): Promise<number> {
  return invoke<number>("clipboard_group_delete", { name });
}

export function clipboardSuggestions(limit?: number): Promise<ClipSuggestion[]> {
  return invoke<ClipSuggestion[]>("clipboard_suggestions", { limit });
}

/** 采纳=写入分组；忽略=永久静默（两者语义不同，忽略绝不改分组） */
export function clipboardSuggestionApply(ids: string[], accept: boolean): Promise<number> {
  return invoke<number>("clipboard_suggestion_apply", { ids, accept });
}

export function clipboardStats(): Promise<ClipStats> {
  return invoke<ClipStats>("clipboard_stats");
}

// ---------------- 加密备份导出/导入（docs/impl/09 §8.2 T-B3-9）----------------

/** 导出回执：path 由宿主拼在 `{appData}/export/` 白名单目录里，前端只能读不能选 */
export interface ClipExportResult {
  path: string;
  entries: number;
  secrets: number;
  images_skipped: number;
}

/** 导入回执：duplicates 就是"合并"这件事的可见证据，不谎报成新增 */
export interface ClipImportReport {
  imported: number;
  duplicates: number;
  secrets: number;
  images_skipped: number;
}

/** 口令加密导出（含敏感行时口令至少 8 字符，否则 CLIPBOARD_EXPORT_001 拒而非静默丢行） */
export function clipboardExport(
  passphrase: string,
  includeSecrets: boolean,
): Promise<ClipExportResult> {
  return invoke<ClipExportResult>("clipboard_export", { passphrase, includeSecrets });
}

/** 口令解密导入：路径来自输入框（全仓无 dialog/fs 插件），任何一步失败都不落半套 */
export function clipboardImport(
  path: string,
  passphrase: string,
): Promise<ClipImportReport> {
  return invoke<ClipImportReport>("clipboard_import", { path, passphrase });
}

// ---------------- 敏感库按需揭示（docs/impl/09 §8.2 T-B3-5）----------------

/** 揭示结果：明文只活在这一次响应里，列表侧恒为掩码 */
export interface ClipReveal {
  id: string;
  text: string;
}

/** 敏感条目明文的唯一出口（clipboard_get 对此类行返回 CLIPBOARD_GET_001，宿主侧写审计） */
export function clipboardSecretReveal(id: string): Promise<ClipReveal> {
  return invoke<ClipReveal>("clipboard_secret_reveal", { id });
}
/** 读模块配置 schema（设置中心自动渲染） */
export function hostConfigSchema(module: string): Promise<Record<string, unknown>> {
  return invoke("host_config_schema", { module });
}

// ---------------- 截图 IPC（docs/impl/03 P8 DTO 对齐）----------------

export interface TaskStartDto {
  task_id: string;
  /** 本次任务定位矩形（物理像素），覆盖层窗口定位用：
   *  全屏轨 = 虚拟桌面原点与整幅；窗口轨（T-B4-4）= 目标窗的 GetWindowRect */
  x: number;
  y: number;
  width: number;
  height: number;
  /**
   * 完成后动作链（T-B4-8）：= 宿主侧 `effective_actions(cfg, [])`，覆盖层「完成」钮按它传动作。
   * 覆盖层因此不需要读设置命令（§9.1-⑪）。运行时可能是 `undefined`（升级前的旧载荷、
   * URL 参数直进的覆盖层），读取处一律过 `completeActions()` 的 `?? []` 兼容臂。
   */
  default_actions: string[];
}

/** 可截取窗口（T-B4-4 `screenshot_windows` 出口，与宿主端口结构同名同型） */
export interface WindowTargetDto {
  hwnd: number;
  title: string;
  x: number;
  y: number;
  width: number;
  height: number;
  /** 最小化窗照样进表并打标：下拉把它排掉，宿主在被选到时明说拒绝 */
  minimized: boolean;
}

export interface TaskInfoDto {
  task_id: string;
  /** shot | ocr */
  mode: string;
  width: number;
  height: number;
  png_b64: string;
  /**
   * 窗口轨句柄（T-B4-4）；`null`/缺省 = 全屏轨。覆盖层两条装载路径（预热事件、URL 回退）
   * 都必然取一次帧，所以这条事实挂在这里而不是 `TaskStartDto`——一个载体、两条路同一个判据。
   */
  hwnd?: number | null;
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
  kind:
    | "pen"
    | "rect"
    | "ellipse"
    | "line"
    | "arrow"
    | "highlight"
    | "text"
    | "mosaic"
    | "number"
    | "blur";
  color: string;
  width: number;
  points: [number, number][];
  text?: string | null;
  seq?: number | null;
  /** z 序（小在下先绘制）；可选=旧历史条目与手改 JSON 零迁移可读（Rust 侧 serde default 0） */
  layer?: number;
  /** 锁定：仍绘制，点选穿透 */
  locked?: boolean;
  /** 形状类是否实心（缺省 false = 旧条目仍是描边框） */
  fill?: boolean;
  /** 笔画透明度 0..1（缺省 1 = 旧条目不透明；独立键而非 #RRGGBBAA 字符串扩展） */
  alpha?: number;
}


/**
 * 美化导出参数（D-29 B4 T-B4-6）：给截图加一张"渐变卡片"——内边距 + 圆角 + 投影 + 渐变底。
 * 字段一一对应 Rust `screenshot_core::beautify::BeautifySpec`。
 */
export interface BeautifySpecDto {
  /** 圆角半径（物理像素，0 = 直角） */
  radius: number;
  /** 内边距：源图在卡片内居中，四边各留 padding 像素 */
  padding: number;
  /** 投影：卡片下方额外 `BEAUTIFY_SHADOW_SPREAD` 行渐隐阴影 */
  shadow: boolean;
  /** 渐变起色 `#RRGGBB`（非法值宿主点名报错，不回落） */
  bg_from: string;
  /** 渐变止色 `#RRGGBB` */
  bg_to: string;
}

export interface FinishRequestDto {
  /** 覆盖层 canvas 合成后的最终图（PNG Base64）——必填，预览即导出 */
  image_b64: string;
  /** save | copy | pin | ocr；空 = 应用设置中的默认动作 */
  actions: string[];
  pin_x?: number | null;
  pin_y?: number | null;
  annotations?: AnnotationDto[];
  /**
   * 本次保存的导出格式（"png" | "jpeg" | "webp"）。**不传 = 跟随设置里的 `format`**；
   * 传未知值宿主点名报错而不是回落默认（由 `finishRequestBody` 单点组装，
   * 见 src/windows/overlay/exportFormats.ts）。
   */
  format?: string;
  /** 美化参数（T-B4-6）：不传 = 不美化；在场即在动作循环之前作用于最终图 */
  beautify?: BeautifySpecDto | null;
}

export interface FinishDto {
  file: string | null;
  pin_id: string | null;
  /**
   * 只预览不落盘时的成品（PNG Base64）。**仅 `screenshot_beautify_apply` 传空 actions 时出现**，
   * `screenshot_finish` 恒无此键——所以前端读取处一律按可选处理。
   */
  preview_b64?: string | null;
  /**
   * 链式上传（`post_actions` 里的 `upload`）拿到的直链（T-B4-9）。
   * 没有该动作 / 目标未启用 / 上传失败时都是缺省——失败另有 `screenshot.upload_failed`
   * 事件点名是哪一条、为什么，这里不做第二个报错口。
   */
  link?: string | null;
}

/**
 * 滚动截图步进一条（T-B4-5）。`segments > 1` 只可能出现在降级之后：
 * 相邻两帧认不出共同行时，前一段整体封盘、这一帧另起一段，一帧都不丢。
 */
export interface ScrollStepDto {
  segments: number;
  /** 累计高度（物理像素）：数字不动就是这一帧没加上，比"事后发现长图缺一截"便宜 */
  height: number;
  degraded: boolean;
  /** 本次新追加那截的 PNG Base64（与产物同源：同一偏移算出来的尾巴） */
  preview_b64: string;
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

/**
 * 单条截图历史的字节出口（D-29 B0-2 缩略图 / T-B4-1 标注 / T-B4-7 格式声明）。
 *
 * `png_b64` 是历史字段名，实为磁盘文件**原始字节**的 Base64：自 T-B4-7 起编码由
 * `format` 声明（宿主按魔数嗅探），拼 data URL 用它而不是认后缀——后缀不再恒 `.png`。
 */
export interface ShotDataDto {
  id: string;
  png_b64: string;
  /** 嗅探出的 MIME：image/png | image/jpeg | image/webp | unknown（坏文件） */
  format: string;
  /** 旧行 NULL → 空表（不是报错）；次序为宿主侧 layer 升序 */
  annotations: AnnotationDto[];
}


/**
 * 一个已注册的上传目标（T-B4-9）。`endpoint_display` **已去掉 query/fragment**——
 * 自建端点常把 token 挂在查询串上，原样回显等于把凭据印在设置界面上。
 */
export interface UploadTargetInfoDto {
  id: string;
  label: string;
  endpoint_display: string;
  enabled: boolean;
}

/** 启动截图：缺省抓全屏，给出 hwnd 则只截该窗（PrintWindow，被遮挡也抓得全） */
export function screenshotStart(mode: "shot" | "ocr", hwnd?: number): Promise<TaskStartDto> {
  return invoke("screenshot_start", { mode, hwnd });
}
/** 可截取窗口表（「截取窗口」下拉；跨应用窗口标题面，main 窗独占） */
export function screenshotWindows(): Promise<WindowTargetDto[]> {
  return invoke("screenshot_windows");
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
/** 滚动截图：按选区矩形抓首帧开一次会话（覆盖层编辑阶段） */
export function screenshotScrollBegin(rect: ConfirmRect): Promise<string> {
  return invoke("screenshot_scroll_begin", { rect });
}
/** 滚动截图：用户滚过一段后追加一帧（宿主自己对重叠区，对不上就分段） */
export function screenshotScrollAppend(id: string): Promise<ScrollStepDto> {
  return invoke("screenshot_scroll_append", { id });
}
/** 滚动截图：收束会话，每段各走一次既有动作通路（分段即各存一图） */
export function screenshotScrollFinish(id: string, actions: string[]): Promise<FinishDto> {
  return invoke("screenshot_scroll_finish", { id, actions });
}
/** 滚动截图：放弃会话（内存里的带子立刻归还） */
export function screenshotScrollDiscard(id: string): Promise<void> {
  return invoke("screenshot_scroll_discard", { id });
}
/** 截图历史分页 */
export function screenshotHistoryList(page: number, size: number): Promise<ShotPageDto> {
  return invoke("screenshot_history_list", { query: { page, size } });
}
/** 历史截图字节（真缩略图，D-29 B0-2） */
export function screenshotHistoryGet(id: string): Promise<ShotDataDto> {
  return invoke("screenshot_history_get", { id });
}
/** 历史截图再复制进系统剪贴板 */
export function screenshotHistoryCopy(id: string): Promise<void> {
  return invoke("screenshot_history_copy", { id });
}
/**
 * 删一条历史：历史行与磁盘上那张图一起移除，**不进回收站、不可撤销**（T-B4-14）。
 * 宿主先删行后删文件，文件删不掉只记日志、不回滚已删的行；id 不存在则报错点名该 id，
 * 不会谎称删好了。
 */
export function screenshotHistoryDelete(id: string): Promise<void> {
  return invoke("screenshot_history_delete", { id });
}
/**
 * 对既有历史条目做美化导出（T-B4-6）。`actions` 与 `screenshot_finish` 同一套词表
 * （save/copy/pin/ocr），**传空数组 = 只要预览**：返回 `preview_b64`，零落盘、零剪贴板写、
 * 零历史新增。美化产物是派生物，不新开历史行。
 */
export function screenshotBeautifyApply(
  id: string,
  spec: BeautifySpecDto,
  actions: string[],
): Promise<FinishDto> {
  return invoke("screenshot_beautify_apply", { id, spec, actions });
}
/** 全部贴图（启动恢复） */
export function screenshotPins(): Promise<PinDto[]> {
  return invoke("screenshot_pins");
}
/** 已注册的上传目标表（T-B4-9：纯读，一个字节都不发出去） */
export function screenshotUploadTargets(): Promise<UploadTargetInfoDto[]> {
  return invoke("screenshot_upload_targets");
}
/**
 * 把某条历史截图传到已注册端点，换回一条直链（T-B4-9）。
 *
 * `headerValue` 是**这一次调用**的凭据值：它不进配置、不进日志，本次 invoke 结束即
 * 无处可寻（§9.1-⑩ 红线）。端点没配请求头名时传 `null`——面板在这种档上根本不给输入框。
 */
export function screenshotUpload(id: string, headerValue: string | null): Promise<string> {
  return invoke("screenshot_upload", { id, headerValue });
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
  /** 本次的显式覆盖语言（BCP-47）；空 = 跟随设置里的偏好语言（非"无偏好"） */
  langs?: string[];
  source_task_id?: string | null;
}

/** 运行态 OCR 配置快照（ocr_config_get 只读；写侧唯一入口仍是 host_config_set） */
export interface OcrConfigDto {
  langs: string[];
  preferred_engine: string;
  /** Tesseract CLI 第二引擎四键（T-B4-11）：默认关，关着时它不出现在引擎状态面 */
  tesseract_enabled: boolean;
  tesseract_exe: string;
  tesseract_data_dir: string | null;
  tesseract_timeout_ms: number;
  /** 译文目标语言（T-B4-13 槽位）：空串 = 不译 */
  translate_target_lang: string;
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
  /** 本次实际所用引擎是否真的报置信度：false 时 lines[].confidence 是"未知"的占位值，
   *  面板据此显示"未提供"而不是把 1.0 渲染成 100.0%（T-B4-13 / §9.1-⑫(b)） */
  engines_report_confidence: boolean;
  /** 译文（后端 skip null：未设目标语言/未启用时这两个键根本不存在） */
  translate?: string | null;
  /** 翻译失败原因（识别照常成功，原因照常可见——T-B4-13 不吞错面） */
  translate_error?: string | null;
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
/** 运行态配置快照（T-B4-10：面板显示"设置里的默认"，读模块内存态，不开第二写口） */
export function ocrConfigGet(): Promise<OcrConfigDto> {
  return invoke("ocr_config_get");
}
/** 导出格式白名单（与 Rust `ocr_core::export::EXPORT_FORMATS` 同集：白名单外后端点名拒写） */
export type OcrExportFormat = "txt" | "md";

/** 合并文本导出（T-B4-12）：只交文本与格式，目录由宿主拼（返回落盘路径） */
export function ocrExport(text: string, format: OcrExportFormat): Promise<string> {
  return invoke("ocr_export", { text, format });
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

/**
 * 剪贴板内容跨机投影（host_core ports.rs ClipContent，serde 外部 tagging：
 * JSON 恰为单键对象 `{Text|Image|Files}`，键名区分大小写）。
 * `html` 在 TS 侧可选、上线必带 null——Rust `Option` 字段缺键即反序列化失败；
 * `bytes` 是 Arc<[u8]> 的 JSON 数字数组（大图请走 send_file，超 CLIP_MAX 报 KVM_TRANSFER_006）。
 */
export type ClipContentDto =
  | { Text: { text: string; html?: string | null } }
  | { Image: { format: string; width: number; height: number; bytes: number[] } }
  | { Files: { paths: string[] } };

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
/** 发送剪贴板内容（Text/Image；Files 变体后端不支持单帧推送，逐文件走 kvmSendFile） */
export function kvmSendClip(deviceId: string, content: ClipContentDto): Promise<void> {
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

/** 保险库头部（vault-core crypto.rs VaultHeader；明文可存盘，机密全在 wrapped_dek） */
export interface VaultHeaderDto {
  version: number;
  vault_id: string;
  kdf: { algo: string; m_cost_kib: number; t_cost: number; p_cost: number; salt_b64: string };
  wrapped_dek: { nonce_b64: string; ct_b64: string };
  /** V4 Hello 免密信封（D-24；未启用 = null） */
  hello?: { wrapped_dek_b64: string; verifier: { nonce_b64: string; ct_b64: string } } | null;
}

/** 密码库状态（三态：uninitialized / locked / unlocked） */
export interface VaultStatusDto {
  state: "uninitialized" | "locked" | "unlocked";
  lockout_remaining_secs: number;
  /** 头部快照（KDF 参数 / vault_id，无机密） */
  kdf: VaultHeaderDto | null;
  /** V4：免密路径已启用 */
  hello_enabled: boolean;
  /** V4：连续校验失败熔断（本进程仅允许主密码解锁） */
  hello_forced: boolean;
  /** 本机 Windows Hello 可用（false = 隐藏免密开关） */
  hello_available: boolean;
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
export function vaultChangeMasterPassword(oldPassword: string, newPassword: string): Promise<VaultHeaderDto> {
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

// ---- V4 Windows Hello 免密 / V5 自动锁定（D-24）----

/** 启用免密解锁（会弹一次系统 Hello 校验窗） */
export function vaultHelloEnable(): Promise<NonNullable<VaultStatusDto["kdf"]>> {
  return invoke("vault_hello_enable");
}
/** 关闭免密解锁 */
export function vaultHelloDisable(): Promise<NonNullable<VaultStatusDto["kdf"]>> {
  return invoke("vault_hello_disable");
}
/** 免密解锁（Cooling 或熔断期由后端拒绝） */
export function vaultHelloUnlock(): Promise<void> {
  return invoke("vault_hello_unlock");
}
/** 主窗口失焦/聚焦上报：驱动后端失焦自动锁定计时线 */
export function vaultNotifyBlur(blurred: boolean): Promise<void> {
  return invoke("vault_notify_blur", { blurred });
}
/** 复制密码字段（后端走回写窗口 + 到期定时清除，D-24/D-10） */
export function vaultCopyPassword(entryId: string, fieldKey: string): Promise<void> {
  return invoke("vault_copy_password", { entryId, fieldKey });
}

// ---------------- 文件与存储（docs/impl/05 F，M6）----------------

export interface FileEntryDto {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified_ms: number;
  ext: string;
  hidden: boolean;
}

export type FileSortKey = "name" | "size" | "modified" | "type";

export interface DriveInfoDto {
  letter: string;
  path: string;
  free_bytes: number;
  total_bytes: number;
}

export type ConflictPolicyDto = "ask" | "skip" | "overwrite" | "rename";

export type FileOpKind = "copy" | "move" | "delete" | "compress" | "extract";

export interface OpSpecDto {
  kind: FileOpKind;
  srcs: string[];
  dst: string;
  policy: ConflictPolicyDto;
  recycle?: boolean;
}

export interface ConflictItemDto {
  name: string;
  dst: string;
}

export interface FileEnqueueDto {
  op_id: string | null;
  conflicts: ConflictItemDto[];
}

export type OpStateDto = "Queued" | "Running" | "Paused" | "Done" | "Failed" | "Canceled";

export interface OpProgressDto {
  op_id: string;
  kind: FileOpKind;
  state: OpStateDto;
  current: string;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  error: string | null;
}

export interface PendingOpDto {
  op_id: string;
  kind: FileOpKind;
  srcs: string[];
  dst: string;
  policy: ConflictPolicyDto;
  recycle: boolean;
  file_index: number;
  bytes_done: number;
  created_ms: number;
}

export type PreviewDto =
  | { kind: "text"; content: string; truncated: boolean }
  | { kind: "image"; data_url: string; width: number; height: number }
  | { kind: "shell"; data_url: string; width: number; height: number }
  | { kind: "unsupported"; reason: string };

export interface FileHitDto {
  path: string;
  score: number;
}

export interface SearchResultDto {
  hits: FileHitDto[];
  degraded: boolean;
}

export interface DriverInfoDto {
  id: string;
  label: string;
  roots: string[];
}

export type RenameCaseDto = "none" | "lower" | "upper";

export interface RenameRuleDto {
  template: string;
  regex?: string | null;
  replacement?: string;
  case?: RenameCaseDto;
  start?: number;
}

export interface RenamePlanDto {
  from: string;
  to: string;
  conflict: boolean;
}

export function fileDrives(): Promise<DriveInfoDto[]> {
  return invoke("file_drives");
}
export function fileList(path: string, sort?: FileSortKey, asc?: boolean): Promise<FileEntryDto[]> {
  return invoke("file_list", { path, sort, asc });
}
export function fileBreadcrumbs(path: string): Promise<[string, string][]> {
  return invoke("file_breadcrumbs", { path });
}
export function fileMkdir(path: string): Promise<void> {
  return invoke("file_mkdir", { path });
}
export function fileRenameEntry(from: string, to: string): Promise<void> {
  return invoke("file_rename_entry", { from, to });
}
export function fileEnqueue(spec: OpSpecDto): Promise<FileEnqueueDto> {
  return invoke("file_enqueue", { spec });
}
export function fileOpsActive(): Promise<OpProgressDto[]> {
  return invoke("file_ops_active");
}
export function fileOpsPending(): Promise<PendingOpDto[]> {
  return invoke("file_ops_pending");
}
export function fileOpPause(opId: string): Promise<void> {
  return invoke("file_op_pause", { opId });
}
export function fileOpResume(opId: string): Promise<string> {
  return invoke("file_op_resume", { opId });
}
export function fileOpCancel(opId: string): Promise<void> {
  return invoke("file_op_cancel", { opId });
}
export function fileOpDropPending(opId: string): Promise<boolean> {
  return invoke("file_op_drop_pending", { opId });
}
export function filePreview(path: string): Promise<PreviewDto> {
  return invoke("file_preview", { path });
}
export function fileSearch(query: string, limit?: number, root?: string | null): Promise<SearchResultDto> {
  return invoke("file_search", { query, limit, root });
}
export function fileDrivers(): Promise<DriverInfoDto[]> {
  return invoke("file_drivers");
}
export function fileRenamePlan(
  dir: string,
  names: string[],
  rule: RenameRuleDto,
): Promise<RenamePlanDto[]> {
  return invoke("file_rename_plan", { dir, names, rule });
}
export function fileRenameApply(plans: RenamePlanDto[]): Promise<number> {
  return invoke("file_rename_apply", { plans });
}

// ======================== 代理（M7 PR，docs/impl/05） ========================

export interface ProxyKernelCapsDto {
  tun: boolean;
  policy_groups: boolean;
  external_controller: boolean;
}

/** 单内核注册表条目（T-B2-2；UI 只按 caps 渲染能力，禁内核特例分支） */
export interface ProxyKernelInfoDto {
  id: string;
  display_name: string;
  installed: boolean;
  version: string | null;
  running: boolean;
  caps: ProxyKernelCapsDto;
  supported_kinds: string[];
}

/** 单 geo 数据资产条目（T-B2-10；GEO_ASSETS 数据驱动，UI 禁资产特例分支） */
export interface ProxyArtifactInfoDto {
  id: string;
  label: string;
  installed: boolean;
  version: string | null;
}

export interface ProxyStatusDto {
  mode: "off" | "system" | "tun";
  kernel_running: boolean;
  kernel_id: string | null;
  inbound_port: number;
  nodes_total: number;
  subs_total: number;
  admin: boolean;
  wintun_installed: boolean;
  kernel_installed: boolean;
  kernel_version: string | null;
  has_backup: boolean;
  restored_last_run: boolean;
  /** 选定内核 id（proxy_state.json 持久化；内核卡高亮） */
  kernel: string;
  /** 全部已注册内核的装机/能力清单 */
  kernels: ProxyKernelInfoDto[];
  /** geo 数据资产装机清单（内核区安装钮数据源） */
  artifacts: ProxyArtifactInfoDto[];
  /** 手动选定的出口节点 [sub_id, tag]；null = 自动（urltest 组自选） */
  selected_node: [string, string] | null;
  /** 选定节点已被订阅更新删除（sticky 展示 + 消费侧回落首节点） */
  selected_stale: boolean;
}

/** 出口自检结果（T-B2-11）：经本地 mixed 代理 GET gstatic 204；
 * 内核未运行/TUN 态后端直接 BadState 错误上抛，不会给假 dto */
export interface ProxyEgressProbeDto {
  ok: boolean;
  /** 往返毫秒；null = 请求未得出（连接层失败） */
  ms: number | null;
  /** HTTP 状态码；null = 未收到响应 */
  status: number | null;
}

/** 订阅标准头流量信息（T-B2-10；面板未发头 = null，UI 不谎显） */
export interface ProxyTrafficDto {
  upload: number;
  download: number;
  left: number;
  /** 到期时刻毫秒；0 = 未提供 */
  expire_ms: number;
}

export interface ProxySubDto {
  id: string;
  name: string;
  url: string;
  updated_ms: number;
  node_count: number;
  traffic: ProxyTrafficDto | null;
  /** 面板建议刷新间隔（profile-update-interval，分钟） */
  interval_min: number | null;
  /** If-None-Match 条件请求指纹（后端透明消费，UI 仅展示"上次更新"） */
  etag: string | null;
}

export interface ProxyNodeDto {
  tag: string;
  kind: string;
  server: string;
  port: number;
  sub_id: string;
  /** Clash YAML 订阅内该节点所属 proxy-groups 组名（T-B2-8；URI 订阅恒空） */
  groups: string[];
}

export interface ProxyNodeDelayDto {
  tag: string;
  sub_id: string;
  /** TCP 连接延迟毫秒；null = 3s 超时不可达 */
  ms: number | null;
}

export interface ProxyLogLineDto {
  ts_ms: number;
  text: string;
}

export interface ProxyManifestDto {
  kernel_id: string;
  kernel_version: string;
  sha256: string;
  installed_at: number;
  channel: string;
}

export function proxyStatus(): Promise<ProxyStatusDto> {
  return invoke("proxy_status");
}
/** 安装/更新指定内核（T-B2-3 参数化：安装钮点名内核，version 空=默认版本；非 sing-box 后端触网前如实拒） */
export function proxyKernelInstall(
  kernel: string,
  version?: string | null,
): Promise<ProxyManifestDto> {
  return invoke("proxy_kernel_install", { kernel, version });
}
/** 内核重启（T-B2-3）：仅运行中有效；mode/kernel 不变，起新失败后端归零为关闭态 */
export function proxyKernelRestart(): Promise<void> {
  return invoke("proxy_kernel_restart");
}
export function proxyWintunInstall(): Promise<void> {
  return invoke("proxy_wintun_install");
}
/** 安装 geo 数据资产（T-B2-10 Binary 通道）：ackPin 为篡改确认位，
 * UI 确认通道归 B9，本批恒缺省（后端保证缺省不因缺 UI 而静默放行） */
export function proxyArtifactInstall(artifact: string): Promise<ProxyManifestDto> {
  return invoke("proxy_artifact_install", { artifact, ackPin: null });
}
/** 选定/切换代理内核：未运行只落选择；运行中新核起、失败后端自动回滚旧核并上抛原错 */
export function proxyKernelSelect(kernel: string): Promise<void> {
  return invoke("proxy_kernel_select", { kernel });
}
export function proxySubs(): Promise<ProxySubDto[]> {
  return invoke("proxy_subs");
}
export function proxySubAdd(name: string, url: string): Promise<ProxySubDto> {
  return invoke("proxy_sub_add", { name, url });
}
export function proxySubRemove(id: string): Promise<boolean> {
  return invoke("proxy_sub_remove", { id });
}
export function proxySubUpdate(id: string): Promise<ProxySubDto> {
  return invoke("proxy_sub_update", { id });
}
export function proxyNodes(): Promise<ProxyNodeDto[]> {
  return invoke("proxy_nodes");
}
export function proxyDirectRules(): Promise<string[]> {
  return invoke("proxy_direct_rules");
}
export function proxySetDirectRules(rules: string[]): Promise<void> {
  return invoke("proxy_set_direct_rules", { rules });
}

/** 分流规则 v2 单行（T-B2-9；T-B2-10 扩 geo 档；枚举串与后端 rules.rs 白名单单一真源） */
export interface ProxyRuleV2Dto {
  kind: "domain" | "suffix" | "keyword" | "ip_cidr" | "process" | "geo_site" | "geo_ip";
  pattern: string;
  target: "direct" | "proxy" | "block";
  enabled: boolean;
}

/** 分流规则 v2 全表：规则 + 兜底 final + 全局分流模式 */
export interface ProxyRulesV2Dto {
  rules: ProxyRuleV2Dto[];
  /** 兜底出口（route_mode=rule 时生效） */
  final_target: "proxy" | "direct" | "block";
  /** global=全部走代理（规则跳过）；rule=规则分流；direct_all=全直连透明兜底档 */
  route_mode: "global" | "rule" | "direct_all";
}

export function proxyRulesGet(): Promise<ProxyRulesV2Dto> {
  return invoke("proxy_rules_get");
}
export function proxyRulesSet(rules: ProxyRulesV2Dto): Promise<void> {
  return invoke("proxy_rules_set", { rules });
}
export function proxySetMode(mode: "off" | "system" | "tun"): Promise<void> {
  return invoke("proxy_set_mode", { mode });
}
export function proxyDelayTest(): Promise<ProxyNodeDelayDto[]> {
  return invoke("proxy_delay_test");
}
/** 选定/切换出口节点（T-B2-11）：后端 NotFound 拒幽灵节点；运行中换点即重启生效 */
export function proxyNodeSelect(subId: string, tag: string): Promise<void> {
  return invoke("proxy_node_select", { subId, tag });
}
/** 切回自动出口（urltest 组自选） */
export function proxyNodeAuto(): Promise<void> {
  return invoke("proxy_node_auto");
}
/** 出口自检：本地 mixed 代理 → gstatic 204；关闭态/TUN 后端 BadState 如实上抛 */
export function proxyEgressProbe(): Promise<ProxyEgressProbeDto> {
  return invoke("proxy_egress_probe");
}
export function proxyLogs(limit?: number): Promise<ProxyLogLineDto[]> {
  return invoke("proxy_logs", { limit });
}

// ======================== 桌面效率（M8 D，docs/impl/05） ========================

export type DesktopItemKind = "app" | "action";

export interface DesktopIndexItemDto {
  id: string;
  name: string;
  kind: DesktopItemKind;
  path: string;
  source: string;
  topic?: string | null;
  payload?: unknown;
}

export interface DesktopLauncherHitDto {
  id: string;
  name: string;
  kind: DesktopItemKind;
  path: string;
  source: string;
  topic?: string | null;
  payload?: unknown;
  /** 综合打分（0,1] */
  score: number;
}

export interface DesktopTidyItemDto {
  name: string;
  path: string;
  category: string;
}

export interface DesktopTidyPlanDto {
  groups: [string, DesktopTidyItemDto[]][];
  total: number;
}

export interface DesktopNoteDto {
  id: string;
  content: string;
  tags: string[];
  remind_at: number | null;
  reminded: boolean;
  done: boolean;
  created_ms: number;
}

export function desktopLauncherSearch(query: string): Promise<DesktopLauncherHitDto[]> {
  return invoke("desktop_launcher_search", { query });
}
export function desktopLauncherLaunch(id: string): Promise<void> {
  return invoke("desktop_launcher_launch", { id });
}
export function desktopLauncherStatus(): Promise<[boolean, number]> {
  return invoke("desktop_launcher_status");
}
export function desktopLauncherReindex(): Promise<number> {
  return invoke("desktop_launcher_reindex");
}
export function desktopTidyPlan(): Promise<DesktopTidyPlanDto> {
  return invoke("desktop_tidy_plan");
}
export function desktopTidyApply(): Promise<[number, number]> {
  return invoke("desktop_tidy_apply");
}
export function desktopTidyRestore(): Promise<number> {
  return invoke("desktop_tidy_restore");
}
export function desktopTidyStatus(): Promise<boolean> {
  return invoke("desktop_tidy_status");
}
export function desktopNoteAdd(content: string): Promise<DesktopNoteDto> {
  return invoke("desktop_note_add", { content });
}
export function desktopNoteList(includeDone: boolean): Promise<DesktopNoteDto[]> {
  return invoke("desktop_note_list", { includeDone });
}
export function desktopNoteDone(id: string, done: boolean): Promise<boolean> {
  return invoke("desktop_note_done", { id, done });
}
export function desktopNoteRemove(id: string): Promise<boolean> {
  return invoke("desktop_note_remove", { id });
}
export function desktopNotesDue(): Promise<DesktopNoteDto[]> {
  return invoke("desktop_notes_due");
}

// ======================== 文本与 PDF（M9 E，docs/impl/06） ========================

export type EditorEncodingKind = "utf8" | "utf8bom" | "utf16le" | "gbk" | "latin1";
export type EditorEol = "crlf" | "lf";

export interface EditorSessionInfoDto {
  id: string;
  path: string;
  name: string;
  encoding: EditorEncodingKind;
  encoding_label: string;
  eol: EditorEol;
  /** 混合行尾（保存将整文件统一——UI 需明示） */
  eol_mixed: boolean;
  dirty: boolean;
  size: number;
  /** >5MB：关语法高亮（E2） */
  big_file: boolean;
  /** >50MB：只读 */
  readonly: boolean;
}

export interface PdfInfoDto {
  pages: number;
  size: number;
}

export interface PdfOpResultDto {
  output: string;
  pages: number;
  size: number;
}

export function editorOpen(path: string): Promise<EditorSessionInfoDto> {
  return invoke("editor_open", { path });
}
export function editorContent(id: string): Promise<string> {
  return invoke("editor_content", { id });
}
export function editorUpdate(id: string, content: string): Promise<boolean> {
  return invoke("editor_update", { id, content });
}
export function editorSave(id: string): Promise<EditorSessionInfoDto> {
  return invoke("editor_save", { id });
}
export function editorSaveAs(id: string, target: string): Promise<EditorSessionInfoDto> {
  return invoke("editor_save_as", { id, target });
}
export function editorAutosave(id: string, content: string): Promise<boolean> {
  return invoke("editor_autosave", { id, content });
}
export function editorClose(id: string): Promise<boolean> {
  return invoke("editor_close", { id });
}
export function editorSessions(): Promise<EditorSessionInfoDto[]> {
  return invoke("editor_sessions");
}
export function pdfInfo(path: string): Promise<PdfInfoDto> {
  return invoke("pdf_info", { path });
}
export function pdfMerge(inputs: string[], output: string): Promise<PdfOpResultDto> {
  return invoke("pdf_merge", { inputs, output });
}
export function pdfSplit(path: string, outDir: string): Promise<PdfOpResultDto[]> {
  return invoke("pdf_split", { path, outDir });
}
export function pdfCompress(path: string): Promise<PdfOpResultDto> {
  return invoke("pdf_compress", { path });
}
export function pdfWatermark(path: string, text: string): Promise<PdfOpResultDto> {
  return invoke("pdf_watermark", { path, text });
}

// ======================== 笔记与知识（M10 N，docs/impl/06） ========================

export interface NoteMetaDto {
  path: string;
  title: string;
  tags: string[];
  mtime_ms: number;
  size: number;
}
export interface NoteReadDto {
  content: string;
  meta: NoteMetaDto;
}
export interface NoteLinkDto {
  dst: string;
  dst_path: string;
}
export interface NoteBacklinkDto {
  src: string;
  title: string;
  snippet: string;
}
export interface NoteSyncResultDto {
  added: number;
  updated: number;
  removed: number;
  total: number;
}
export interface NoteCardDto {
  id: string;
  note_path: string | null;
  front: string;
  back: string;
  ef: number;
  interval_days: number;
  reps: number;
  due_ms: number;
}
export interface CanvasNodeDto {
  id: string;
  kind: "note" | "sticky" | "image" | string;
  x: number;
  y: number;
  w: number;
  h: number;
  ref?: string | null;
  text?: string | null;
  src?: string | null;
  label?: string | null;
}
export interface CanvasEdgeDto {
  id: string;
  from: string;
  to: string;
  label?: string | null;
}
export interface CanvasDocDto {
  version: number;
  nodes: CanvasNodeDto[];
  edges: CanvasEdgeDto[];
}

export function notesList(): Promise<NoteMetaDto[]> {
  return invoke("notes_list");
}
export function notesRead(relPath: string): Promise<NoteReadDto> {
  return invoke("notes_read", { relPath });
}
export function notesCreate(relPath: string, content?: string): Promise<NoteMetaDto> {
  return invoke("notes_create", { relPath, content: content ?? null });
}
export function notesWrite(relPath: string, content: string): Promise<void> {
  return invoke("notes_write", { relPath, content });
}
export function notesDelete(relPath: string): Promise<void> {
  return invoke("notes_delete", { relPath });
}
export function notesRename(oldPath: string, newPath: string): Promise<void> {
  return invoke("notes_rename", { oldPath, newPath });
}
export function notesLinks(relPath: string): Promise<NoteLinkDto[]> {
  return invoke("notes_links", { relPath });
}
export function notesBacklinks(relPath: string): Promise<NoteBacklinkDto[]> {
  return invoke("notes_backlinks", { relPath });
}
export function notesSync(): Promise<NoteSyncResultDto> {
  return invoke("notes_sync");
}
export function notesReindex(): Promise<NoteSyncResultDto> {
  return invoke("notes_reindex");
}
export function notesCards(): Promise<NoteCardDto[]> {
  return invoke("notes_cards");
}
export function notesCardCreate(front: string, back: string, notePath?: string): Promise<NoteCardDto> {
  return invoke("notes_card_create", { front, back, notePath: notePath ?? null });
}
export function notesCardDelete(id: string): Promise<boolean> {
  return invoke("notes_card_delete", { id });
}
export function notesReviewQueue(): Promise<NoteCardDto[]> {
  return invoke("notes_review_queue");
}
export function notesReviewGrade(id: string, quality: number): Promise<NoteCardDto> {
  return invoke("notes_review_grade", { id, quality });
}
export function notesCanvasGet(dir: string): Promise<CanvasDocDto> {
  return invoke("notes_canvas_get", { dir });
}
export function notesCanvasSave(dir: string, doc: CanvasDocDto): Promise<void> {
  return invoke("notes_canvas_save", { dir, doc });
}
export function notesCanvasDirs(): Promise<string[]> {
  return invoke("notes_canvas_dirs");
}

// ======================== 终端与运维（M11 T，docs/impl/06） ========================

export type TermKindDto =
  | { kind: "local" }
  | { kind: "wsl"; distro: string }
  | { kind: "ssh"; host: string; port: number; user: string };

export interface TermSessionDto {
  id: string;
  kind: TermKindDto;
  title: string;
  alive: boolean;
  cols: number;
  rows: number;
}
export interface SshAuthDto {
  kind: "password" | "key";
  password?: string;
  key_path?: string;
  passphrase?: string;
}
export interface SshKnownHostDto {
  host: string;
  fingerprint: string;
}
export interface SftpEntryDto {
  name: string;
  is_dir: boolean;
  size: number;
}
export interface DockerContainerDto {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
}

export function termSpawnLocal(shell?: string, cwd?: string, cols = 80, rows = 24): Promise<TermSessionDto> {
  return invoke("term_spawn_local", { shell: shell ?? null, cwd: cwd ?? null, cols, rows });
}
export function termSpawnWsl(distro: string, cols = 80, rows = 24): Promise<TermSessionDto> {
  return invoke("term_spawn_wsl", { distro, cols, rows });
}
export function termWslList(): Promise<string[]> {
  return invoke("term_wsl_list");
}
export function termWrite(sessionId: string, data: string): Promise<void> {
  return invoke("term_write", { sessionId, data });
}
export function termResize(sessionId: string, cols: number, rows: number): Promise<void> {
  return invoke("term_resize", { sessionId, cols, rows });
}
export function termAck(sessionId: string, receivedTotal: number): Promise<void> {
  return invoke("term_ack", { sessionId, receivedTotal });
}
export function termKill(sessionId: string): Promise<void> {
  return invoke("term_kill", { sessionId });
}
export function termSessions(): Promise<TermSessionDto[]> {
  return invoke("term_sessions");
}
export function termSshConnect(
  conn: { host: string; port: number; user: string; auth: SshAuthDto; cols: number; rows: number },
): Promise<TermSessionDto> {
  return invoke("term_ssh_connect", { conn });
}
export function termSshKnownHosts(): Promise<SshKnownHostDto[]> {
  return invoke("term_ssh_known_hosts");
}
export function termSshForgetHost(host: string): Promise<boolean> {
  return invoke("term_ssh_forget_host", { host });
}
export function termSftpList(
  host: string, port: number, user: string, auth: SshAuthDto, path: string,
): Promise<SftpEntryDto[]> {
  return invoke("term_sftp_list", { host, port, user, auth, path });
}
export function termSftpDownload(
  host: string, port: number, user: string, auth: SshAuthDto, remotePath: string, localPath: string,
): Promise<number> {
  return invoke("term_sftp_download", { host, port, user, auth, remotePath, localPath });
}
export function termSftpUpload(
  host: string, port: number, user: string, auth: SshAuthDto, localPath: string, remotePath: string,
): Promise<number> {
  return invoke("term_sftp_upload", { host, port, user, auth, localPath, remotePath });
}
export function termDockerContainers(): Promise<DockerContainerDto[]> {
  return invoke("term_docker_containers");
}
export function termDockerLifecycle(id: string, start: boolean): Promise<void> {
  return invoke("term_docker_lifecycle", { id, start });
}
export function termDockerLogs(id: string, tail: number): Promise<string> {
  return invoke("term_docker_logs", { id, tail });
}

// ======================== 系统管理（M12 SY，docs/impl/06） ========================

export interface PkgSourceDto {
  id: string;
  label: string;
  available: boolean;
}
export interface PkgEntryDto {
  id: string;
  name: string;
  version: string;
  available: string | null;
  source: string;
}
export interface CleanTargetDto {
  id: string;
  label: string;
  dir: string;
  exts: string[];
  need_admin: boolean;
  safe_default: boolean;
  optional: boolean;
}
export interface CleanScanItemDto {
  target_id: string;
  label: string;
  need_admin: boolean;
  safe_default: boolean;
  files: number;
  reclaim_bytes: number;
  skipped_recent: number;
  missing: boolean;
}
export interface DiskPointDto {
  mount: string;
  used: number;
  total: number;
}
export interface MetricsPointDto {
  ts_ms: number;
  cpu: number;
  mem_used: number;
  mem_total: number;
  net_bps: number;
  disks: DiskPointDto[];
}

export function sysPkgSources(): Promise<PkgSourceDto[]> {
  return invoke("sys_pkg_sources");
}
export function sysPkgList(): Promise<PkgEntryDto[]> {
  return invoke("sys_pkg_list");
}
export function sysPkgCmdPreview(source: string, action: string, packageId: string): Promise<string> {
  return invoke("sys_pkg_cmd_preview", { source, action, packageId });
}
export function sysPkgAction(source: string, action: string, packageId: string): Promise<string[]> {
  return invoke("sys_pkg_action", { source, action, packageId });
}
export function sysCleanTargets(): Promise<CleanTargetDto[]> {
  return invoke("sys_clean_targets");
}
export function sysCleanScan(): Promise<CleanScanItemDto[]> {
  return invoke("sys_clean_scan");
}
export function sysCleanExecute(selectedIds: string[], recycle: boolean): Promise<number> {
  return invoke("sys_clean_execute", { selectedIds, recycle });
}
export function sysMetricsHistory(): Promise<MetricsPointDto[]> {
  return invoke("sys_metrics_history");
}

// ======================== WinOps Tweak 引擎（M16 W1，docs/impl/08） ========================

export interface WinopsRegistryActionDto {
  type: "registry";
  key: string;
  value_name: string;
  value_type: string;
  data: { dword?: number; qword?: number; str?: string };
}
// W2 起出现的动作形态（服务/计划任务），前端只读展示
export interface WinopsOpaqueActionDto {
  type: string;
  [k: string]: unknown;
}
export type WinopsActionDto = WinopsRegistryActionDto | WinopsOpaqueActionDto;

export interface WinopsTweakDto {
  id: string;
  name: string;
  category: string;
  description: string;
  requires_admin: boolean;
  /** 维护型 tweak（clear_cache 等）：无「已应用」状态，scan 恒 not_applied（核账③补） */
  maintenance: boolean;
  actions: WinopsActionDto[];
}

/** scan 状态（Rust ScanState snake_case） */
export type WinopsScanState = "applied" | "not_applied" | "needs_admin";

/** Rust Vec<(Tweak, ScanState)> serde 序列化为 [tweak, state] 数组 */
export type WinopsScanItemDto = [WinopsTweakDto, WinopsScanState];

export interface WinopsApplyReportDto {
  tweak_id: string;
  backup: { tweak_id: string; key: string; value_name: string; existed: boolean; old_value: unknown }[];
  verified: boolean;
}

export function winopsCatalog(): Promise<WinopsTweakDto[]> {
  return invoke("winops_catalog");
}
export function winopsScan(): Promise<WinopsScanItemDto[]> {
  return invoke("winops_scan");
}
export function winopsApply(id: string): Promise<WinopsApplyReportDto> {
  return invoke("winops_apply", { id });
}
export function winopsRollback(id: string): Promise<void> {
  return invoke("winops_rollback", { id });
}

/** 导出 WinOps 审计（审计记录 + 备份清单），返回导出文件路径（W7） */
export function winopsAuditExport(): Promise<string> {
  return invoke("winops_audit_export");
}

// ======================== 自动化与拓展（M14 A1–A3，docs/impl/07） ========================

// Trigger/Expr/Action 与 Rust serde 内部 tag 序列化一一对应
export type TriggerDto =
  | { kind: "event"; topic: string }
  | { kind: "startup" }
  | { kind: "schedule"; time: string };

export type CmpOpDto = "eq" | "ne" | "gt" | "lt" | "contains";

export type ExprDto =
  | { op: "leaf"; args: { path: string; cmp: CmpOpDto; value: unknown } }
  | { op: "and"; args: ExprDto[] }
  | { op: "or"; args: ExprDto[] }
  | { op: "not"; args: ExprDto };

export type ActionDto =
  | { kind: "publish"; topic: string; payload: unknown }
  | { kind: "notify"; title: string; body: string }
  | { kind: "open_url"; url: string }
  | { kind: "ipc_command"; module: string; cmd: string; args: unknown }
  | { kind: "run_script"; path: string; func: string };

export interface RuleDto {
  id: string;
  name: string;
  on: TriggerDto;
  when: ExprDto | null;
  then: ActionDto[];
  cooldown_secs: number;
  enabled: boolean;
}

export interface DeadLetterDto {
  id: string;
  rule_id: string;
  rule_name: string;
  action: ActionDto;
  error: string;
  at_ms: number;
}

export function automationRulesList(): Promise<RuleDto[]> {
  return invoke("automation_rules_list");
}
export function automationSaveRule(rule: RuleDto): Promise<void> {
  return invoke("automation_save_rule", { rule });
}
export function automationDeleteRule(id: string): Promise<boolean> {
  return invoke("automation_delete_rule", { id });
}
export function automationToggleRule(id: string, enabled: boolean): Promise<boolean> {
  return invoke("automation_toggle_rule", { id, enabled });
}
export function automationDeadLetters(): Promise<DeadLetterDto[]> {
  return invoke("automation_dead_letters");
}
export function automationReplay(deadId: string, ruleId: string): Promise<void> {
  return invoke("automation_replay", { deadId, ruleId });
}

// 插件管理（M14 A6）
export interface PluginManifestDto {
  id: string;
  name: string;
  version: string;
  api_version: number;
  permissions: string[];
  entry: string;
  func: string;
  sha256: string;
}

export interface PluginInfoDto {
  id: string;
  name: string;
  version: string;
  api_version: number;
  permissions: string[];
  entry: string;
  func: string;
  sha256: string;
  installed: boolean;
}

export function automationPluginsList(): Promise<PluginInfoDto[]> {
  return invoke("automation_plugins_list");
}
export function automationPluginInstall(srcDir: string): Promise<PluginManifestDto> {
  return invoke("automation_plugin_install", { srcDir });
}
export function automationPluginRemove(id: string): Promise<boolean> {
  return invoke("automation_plugin_remove", { id });
}

// ======================== 跨设备同步（M15 SYNC，docs/impl/07） ========================

export interface PairedPeerDto {
  device_id: string;
  device_name: string;
  fingerprint: string;
  pubkey_b64: string;
  paired_at: number;
}

export interface SyncSummaryDto {
  pushed: number;
  pulled_applied: number;
  pulled_lost: number;
  conflicts: number;
}

export interface SyncStatusDto {
  op_count: number;
  port: number;
}

/**
 * 冲突历史行（09 §10.2 T-B5-2）：camelCase 与 sync-core `ConflictEntry` 的
 * `serde(rename_all = "camelCase")` 一一对应（该结构体是单一真源，命令层直接透出）。
 */
export interface SyncConflictDto {
  conflictId: string;
  entity: string;
  entityId: string;
  /** 败方条目时间戳（毫秒） */
  lostTs: number;
  lostDevice: string;
  /** 当时压住它的本机条目 */
  winnerDevice: string;
  winnerTs: number;
  /** 败方内容快照（删除标记即 `{ deleted: true }`） */
  lostValue: { content?: string; title?: string; deleted?: boolean };
  recordedMs: number;
}

/** 恢复结果（只承诺"本机新变更已入流"，不含"对端已回滚"——本机无法保证对端此后不再改） */
export interface SyncRestoreDto {
  conflictId: string;
  entity: string;
  entityId: string;
  opId: string;
  ts: number;
}

export function syncPeers(): Promise<PairedPeerDto[]> {
  return invoke("sync_peers");
}
export function syncStatus(): Promise<SyncStatusDto> {
  return invoke("sync_status");
}
export function syncNow(deviceId: string, addr: string): Promise<SyncSummaryDto> {
  return invoke("sync_now", { deviceId, addr });
}
export function syncConflictsGet(limit: number, offset: number): Promise<SyncConflictDto[]> {
  return invoke("sync_conflicts_get", { limit, offset });
}
export function syncConflictRestore(conflictId: string): Promise<SyncRestoreDto> {
  return invoke("sync_conflict_restore", { conflictId });
}
