# 02 clipboard-core 剪切板中枢细化（代码级）

> 依赖：S1–S7（见 01 文档）｜ crate：`crates/clipboard-core/` ｜ DB：`{appData}/db/clipboard.db`

## 实现步骤总览

| 步骤 | 内容 | 依赖 |
|------|------|------|
| C1 | 类型定义（ClipContent / ClipEntry / Config） | S2 |
| C2 | 存储层（建库/迁移/FTS5/CRUD） | C1 |
| C3 | 捕获管线（监听→过滤→去重→批量落库→事件） | C2, S3(ClipboardPort), S4 |
| C4 | 敏感数据保护（识别/加密/黑名单） | C1, S6 |
| C5 | 智能分组分类器 | C1 |
| C6 | 查询服务（FTS 搜索/分页/堆栈） | C2 |
| C7 | IPC 命令 | C3,C6 |
| C8 | 前端 UI（历史面板/快速面板/store） | C7 |
| C9 | 清理任务（保留期/上限/GC） | C2 |

---

## C1 类型定义

```rust
// crates/clipboard-core/src/types.rs
#[derive(Clone, Serialize, Deserialize)]
pub enum ClipContent {
    Text  { text: String, html: Option<String> },      // html 保留富文本粘贴能力
    Image { format: ImageFormat, size: (u32, u32), bytes: Arc<[u8]> },
    Files { paths: Vec<PathBuf> },
}

#[derive(Clone, Serialize)]
pub struct ClipEntry {
    pub id: String,                // uuid v7
    pub content_type: &'static str,// "text"|"image"|"files"
    pub preview: String,           // 列表预览：文本前 200 字符 / "[图片 1920×1080]" / 文件数
    pub blob_path: Option<String>, // 大内容引用
    pub origin: Origin,            // Local | Remote
    pub source_app: Option<String>,
    pub pinned: bool,
    pub group: Option<&'static str>,   // url|json|code|secret|color|...
    pub secret: bool,              // 命中敏感规则
    pub created_at: i64,
    pub usage_count: u32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClipboardConfig {
    pub max_entries: u32 = 5000,
    pub retention_days: u32 = 30,
    pub capture_images: bool = true,
    pub capture_files: bool = true,
    pub sensitive_filter: bool = true,
    pub auto_group: bool = true,
    pub excluded_apps: Vec<String>,    // "永不记录"黑名单（进程名，如 1password.exe）
}
```

---

## C2 存储层

### SQL schema（DESIGN §4.1 已定，此处补充触发器与迁移）

```sql
-- migration v1（用 rusqlite_migration 组织）
CREATE TABLE clip_entries ( /* 见 DESIGN §4.1，含 origin 列 */ );
CREATE VIRTUAL TABLE clip_fts USING fts5(content, content='clip_entries',
    content_rowid='rowid', tokenize='unicode61');
-- FTS 同步触发器（外部内容表必须手工同步）
CREATE TRIGGER clip_ai AFTER INSERT ON clip_entries BEGIN
  INSERT INTO clip_fts(rowid, content) VALUES (new.rowid, new.content); END;
CREATE TRIGGER clip_ad AFTER DELETE ON clip_entries BEGIN
  INSERT INTO clip_fts(clip_fts, rowid, content) VALUES('delete', old.rowid, old.content); END;
CREATE TRIGGER clip_au AFTER UPDATE ON clip_entries BEGIN
  INSERT INTO clip_fts(clip_fts, rowid, content) VALUES('delete', old.rowid, old.content);
  INSERT INTO clip_fts(rowid, content) VALUES (new.rowid, new.content); END;
```

```rust
// crates/clipboard-core/src/store.rs
pub struct ClipStore { conn: Arc<Mutex<Connection>> }   // 单写连接 + WAL
impl ClipStore {
    pub fn open(db_path: &Path) -> Result<Self, AppError>;      // PRAGMA journal_mode=WAL; foreign_keys=ON; 迁移
    pub fn insert(&self, e: &NewEntry) -> Result<String, AppError>;       // 返回 id；命中 hash 去重则上移置顶并返回既有 id
    pub fn search(&self, q: &SearchQuery) -> Result<Page<ClipEntry>, AppError>;
    pub fn get_blob(&self, id: &str) -> Result<Option<Vec<u8>>, AppError>;
    pub fn pin(&self, id: &str, pinned: bool) -> Result<(), AppError>;
    pub fn delete(&self, id: &str, secure: bool) -> Result<(), AppError>; // secure: blob 覆写后再 unlink
    pub fn clear(&self, keep_pinned: bool) -> Result<u32, AppError>;
    pub fn push_stack(&self, id: &str) -> Result<(), AppError>;
    pub fn pop_stack(&self) -> Result<Option<ClipEntry>, AppError>;
    pub fn purge_older_than(&self, days: u32, keep: u32) -> Result<u32, AppError>; // C9 用
}
```

### 核心算法：去重插入

```sql
-- 伪代码 insert(new):
BEGIN;
SELECT id FROM clip_entries WHERE content_hash = ?new.hash LIMIT 1;
-- 命中：UPDATE created_at = now, usage_count += 1；UPDATE 返回既有 id（相当于"重新复制置顶"）
-- 未命中：INSERT；len(new.content) > 64KB → 内容写 blob 文件，主表存 blob_path + 空 content
COMMIT;
```

### 潜在问题
- rusqlite `Connection` 非 Sync：包 `Mutex` 且**所有 DB 调用放 `spawn_blocking`**，避免阻塞异步运行时。
- FTS5 查询需对用户输入做转义（把 `"` 替换掉，用前缀匹配 `q*`），否则语法错误。
- WAL 模式下 `-wal`/`-shm` 文件随库文件走，备份/迁移时三者一起处理。

---

## C3 捕获管线（核心数据流）

```
WM_CLIPBOARDUPDATE（Port 回调，专用线程）
  → raw_queue(mpsc, cap=64)            // Port 线程只入队，快速返回
  → 管线 worker（spawn_blocking 常驻）:
     ① 过滤：来源进程在 excluded_apps？→ 丢弃
     ② 读取多格式：CF_UNICODETEXT/CF_HTML/CF_DIB/CF_HDROP（win-integration 负责）
     ③ origin 判定：回写窗口期(500ms)内的读取 → Origin::Local 写回事件（防循环，见潜在问题）
     ④ secret 预检（C4）：命中 → 加密 + secret=true
     ⑤ hash 去重 → insert（C2）
     ⑥ 分类（C5）回填 group
     ⑦ 发事件 clipboard.captured { entry: ClipEntry }（前端消费后 IPC 拉详情）
```

```rust
pub struct CapturePipeline {
    rx: mpsc::Receiver<RawClip>,
    store: Arc<ClipStore>,
    classifier: Arc<Classifier>,
    secrets: Arc<SecretFilter>,
    write_back_guard: Mutex<Option<Instant>>,   // C3-③ 回写窗口
}
impl CapturePipeline {
    pub fn run(self) { /* 常驻循环；每轮批量：最多 20 条或 50ms 窗口提交一次事务 */ }
    pub fn mark_write_back(&self);              // ClipboardPort::write 前调用，记录窗口起点
}
```

### 潜在问题（本模块最高风险区）
1. **消息循环线程**：`AddClipboardFormatListener` 要求隐藏窗口 + `GetMessage` 循环，必须放专用 OS 线程，不能在 tokio worker 上跑（win-integration 职责，但集成联调在 C3）。
2. **剪贴板循环**：自己 `write` 回剪贴板会再次触发 `WM_CLIPBOARDUPDATE`。方案即 C3-③ 回写窗口；窗口期内读取直接丢弃。窗口取 500ms 经验值，写成常量便于调整。
3. **读取失败重试**：`OpenClipboard` 可能被其它进程占用（ERROR_ACCESS_DENIED），指数退避重试 3 次（10/50/200ms），仍失败丢弃本次并记 debug 日志（不允许报错打扰用户）。
4. **高频复制风暴**（如脚本循环复制）：入队容量 64，满则丢最旧并计数；这是"当前值最重要"的语义，可接受。
5. **大图**：>64KB 图片必须走 blob；`Arc<[u8]>` 避免 Arc<ClipContent> 克隆时多次拷贝像素。

---

## C4 敏感数据保护

```rust
pub struct SecretFilter { rules: Vec<SecretRule> }   // 编译期构建，正则用 once_cell 预编译
impl SecretFilter {
    /// 返回命中类别；None = 非敏感
    pub fn inspect(&self, text: &str) -> Option<SecretKind>;
}
pub enum SecretKind { PrivateKey, Jwt, ApiKey, CreditCard, IdNumber, PhoneNumber }
```

**规则（按序短路）**：
1. `-----BEGIN .* PRIVATE KEY-----`（PEM 头，5 秒判）
2. JWT：`^eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.` 
3. API Key 特征：`sk-[A-Za-z0-9]{20,}` / `ghp_[A-Za-z0-9]{36}` / `AKIA[0-9A-Z]{16}`
4. 银行卡：Luhn 校验通过的 13–19 位数字串
5. 身份证：18 位 + 校验位验证
6. 手机号：`1[3-9]\d{9}`（仅当整条内容就是手机号时才判敏感，避免误伤普通文本）

**加密存储**：命中 → 内容用 AES-256-GCM 加密（密钥来自 host-keychain，首启生成、DPAPI/Credential Manager 保护）后 Base64 入 `content` 列；`preview` 置为 `[敏感内容 · {kind}]`。解密仅在 `clipboard_get` 明确请求时进行。

**潜在问题**：正则只做特征判定，**禁止**把疑似敏感内容写入日志；`tracing` 事件 payload 走 host-core 脱敏 layer。

---

## C5 智能分组分类器

```rust
pub struct Classifier { rules: Vec<Box<dyn ClassificationRule>> }   // 规则按优先级排序
pub trait ClassificationRule: Send + Sync {
    fn classify(&self, text: &str) -> Option<(&'static str, f32)>;  // (group, confidence)
}
// 内置规则及优先级：Secret(0.99，命中即止) > Url(0.95, starts_with http) >
// Json(0.9, serde_json 解析成功且首字符为 { 或 [) > Color(0.9, #RRGGBB) >
// Code(0.7, 关键词密度: fn/def/class/import/const + 符号密度 > 0.08) > Plain
// 算法：顺序执行规则，首个 confidence ≥ 0.85 即返回；否则取最高分；均 < 0.5 → None
```

### 潜在问题
- Code 规则做启发式即可，不引入语法分析依赖；误判可接受（用户可手动打标签）。
- 长文本（>10KB）跳过 Json/Code 规则，避免正则回溯卡顿。

---

## C6 查询服务

```rust
pub struct SearchQuery { pub text: Option<String>, pub group: Option<&'static str>,
    pub content_type: Option<&'static str>, pub pinned_first: bool, pub page: u32, pub size: u32 /* 默认 50 */ }
// 算法：
// text 有值 → SELECT ... FROM clip_fts JOIN clip_entries USING(rowid)
//            WHERE clip_fts MATCH ? ORDER BY rank, created_at DESC LIMIT ? OFFSET ?
// text 无值 → 主表 ORDER BY pinned DESC, created_at DESC
// 返回 Page { items, total, has_more }；total 用 COUNT(*) 且仅第一页计算
```

## C7 IPC 命令

```rust
clipboard_search(query: SearchQuery) -> Result<Page<ClipEntry>, AppError>
clipboard_get(id) -> Result<ClipDetail, AppError>          // 含解密后内容（secret 需前端二次确认参数 confirm=true）
clipboard_paste(id) -> Result<(), AppError>                // mark_write_back → Port.write
clipboard_pin(id, pinned) / clipboard_delete(id, secure) / clipboard_clear(keep_pinned)
clipboard_stack_push(id) / clipboard_stack_pop() -> Option<ClipEntry>
clipboard_tag_list() / clipboard_tag_add(entry_id, tag)
// 事件（由 C3/前端订阅）：clipboard.captured {entry} | clipboard.deleted {id} | clipboard.cleared
```

## C8 前端 UI

```
src/modules/clipboard/
├── ClipboardPanel.tsx        // 主面板：搜索框 + 分组 Tab + TanStack Virtual 列表
├── ClipboardQuickPanel.tsx   // 快速面板（快捷键呼出，最多显示 9 条，数字键直选）
├── EntryCard.tsx             // 文本=摘要行；图片=缩略图（从 blob 懒加载）；文件=图标+路径
├── store.ts                  // Zustand: entries[], page, group, activeStack
└── ipc.ts                    // 类型化封装上述命令
```

**交互规约**：列表项 Enter=粘贴（调 `clipboard_paste` 后隐藏面板）；Del=删除；Ctrl+P=置顶。空状态文案："复制任意内容后，这里会显示历史记录"。

## C9 清理任务

- 触发：模块 start 时 + 每 6h 定时（tokio interval）。
- 算法：`DELETE FROM clip_entries WHERE pinned=0 AND created_at < now-retention_days`（分批 500 条/事务防长锁）；若总数 > max_entries，按 created_at 升序删除溢出部分；随后 GC blob 目录中无主文件（启动期也可执行）。

## 验收（C 出口）

- [ ] 复制文本/图片/文件三类型均入历史，延迟 P95 < 100ms
- [ ] 相同内容重复复制 → 置顶去重，不新增行
- [ ] PEM/JWT/银行卡 → preview 显示 `[敏感内容]`，DB 中为密文
- [ ] 1password.exe 在黑名单时复制密码不产生记录
- [ ] 自己 paste 写回不产生新记录（防循环）
- [ ] 5000 条数据 FTS 搜索 < 50ms；模块 panic 可从 UI 重启
