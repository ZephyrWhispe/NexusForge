# 03 · 正确性与数据完整性问题详解

> 覆盖 `COR-01`…`COR-31`。每条含：问题描述（含证据）→ 解决方案（步骤 + 代码示例）→ 修复前后对比 → 预防措施。
> 本类问题的共同特征是**静默丢数据/静默出错**：用户看到"成功"，但磁盘/数据库/对端状态已经不一致。因此每条修复都必须附带**断言副作用的测试**（不只是断言返回值）。

---

# COR-01 · 剪贴板 >64KB 文本：blob 不读回，预览与粘贴为空　`P1` `[实测]`

### 位置
- [crates/clipboard-core/src/store.rs:300-309](../../crates/clipboard-core/src/store.rs#L300-L309)（`insert_row`）
- [crates/clipboard-core/src/store.rs:798-838](../../crates/clipboard-core/src/store.rs#L798-L838)（`get_payload`）

### 问题描述

写入侧：>64KB 文本把正文写进 blob 文件，但 `content` 列写**空串**：
```rust
// store.rs:300-309
let (content_col, blob_path): (String, Option<String>) = if c.text.len() > BLOB_THRESHOLD {
    let name = format!("{}.txt", hash);
    let blob = self.blob_dir.join(&name);
    std::fs::write(&blob, c.text).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
    (String::new(), Some(name))            // ← content 列 = ""
} else if c.secret { (String::new(), None) } else { (c.text.to_string(), None) };
```

读取侧：`get_payload` 的文本臂**只看 `content` 列，完全不看 `blob_path`**：
```rust
// store.rs:828-837
_ => {
    let Some(content) = content_opt else { return Ok(None) };
    if secret == 1 { Ok(Some(Payload::SecretB64(content))) } else { Ok(Some(Payload::Text(content))) }
}
```
（对比：`"image"` 臂 [:812-820](../../crates/clipboard-core/src/store.rs#L812-L820) 是读 blob 的——文本臂漏了。）

### 影响

复制一次 >64KB 的纯文本（长日志、证书、JSON）后：
1. 历史列表**预览空白**（`entry_from_row` 用 `content` 生成预览）；
2. `clipboard_paste`/栈粘贴把**空字符串**写回系统剪贴板 → 用户"粘贴"得到空内容；
3. 内容实际只存在于 `blobs/clipboard/<hash>.txt`，且只有 `clipboard_get`（`get_content`）会读它——即**同一份数据不同读口行为不一致**。

### 解决方案

**方案 A（推荐）：写入侧保留截断预览 + 读取侧优先 blob。两者必须同改。**

```rust
// ① 写入侧：content 存"截断预览"，blob_path 存全量（预览列与全量分离）
const PREVIEW_CHARS: usize = 512;
let (content_col, blob_path) = if c.text.chars().count() > PREVIEW_CHARS {
    let name = format!("{}.txt", hash);
    std::fs::write(self.blob_dir.join(&name), c.text).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
    let preview: String = c.text.chars().take(PREVIEW_CHARS).collect();   // 供列表预览
    (preview, Some(name))
} else { (c.text.to_string(), None) };

// ② 读取侧：blob 存在则以 blob 为准（即使 content 有预览，也绝不返回截断内容）
_ => {
    if let Some(blob) = blob_path.as_deref() {
        let bytes = std::fs::read(self.blob_dir.join(blob)).map_err(|e| err("CLIPBOARD_STORAGE_002", e))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        return Ok(Some(if secret == 1 { Payload::SecretB64(text) } else { Payload::Text(text) }));
    }
    let Some(content) = content_opt else { return Ok(None) };
    if secret == 1 { Ok(Some(Payload::SecretB64(content))) } else { Ok(Some(Payload::Text(content))) }
}
```

**方案 B（最小改动）：保持 `content=""`，只在读取侧补 blob 分支并让预览显示"（大文本）"占位。**

> 注意顺序陷阱：**若采用方案 A 的"写预览"，则读取侧必须先判 blob**；否则会粘贴出截断的 512 字。这两处必须作为一次原子改动，并在测试中同时断言"预览非空"与"粘贴内容与原文完全相等"。

**顺带修掉相邻隐患**：`insert_row` 的分支顺序是"先判 blob 再判 secret"（`store.rs:300-306`），意味着**敏感且 >64KB** 的文本会走 blob 分支把**明文写盘**。当前 `NewClip::secret()` 无生产调用点（死码），但应在同一次修复中改为"secret 优先"：

```rust
let (content_col, blob_path) = if c.secret {
    (String::new(), None)                 // 密文由管线层写入（insert_encrypted）
} else if c.text.len() > BLOB_THRESHOLD { ... } else { ... };
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 列表预览（>64KB） | 空 | 前 512 字（或"（大文本）"） |
| 粘贴（>64KB） | **空字符串** | 与原文逐字节相等 |
| 读取一致性 | `get_content` 读 blob / 粘贴读 `content`（两条路不一致） | 所有读口统一"blob 优先" |
| 敏感+大文本 | 明文落 blob（潜在） | 走加密路径，不落明文 |
| 测试 | 无 >64KB 端到端用例 | 新增"写入→预览→粘贴"三段断言 + 敏感大文本负例 |

### 预防措施

1. **"一写多读"数据必须有单一读取函数**：把 blob/content 的读取收敛为 `fn read_text_payload(&self, row) -> Result<String>`，所有消费点（列表、粘贴、导出、同步）都调用它——避免"每个读口各写一遍"（本次缺陷即由此产生）。
2. **阈值分支测试矩阵**：对"文本/图片/文件/敏感 × 小于/大于 64KB"6 种组合建参数化测试（`rstest` 或手写循环），断言"写后读回 == 原值"。
3. **把 `BLOB_THRESHOLD` 的处理写成不变量**：`content IS NULL 或 content = 预览 ⇒ blob_path IS NOT NULL`，并加数据库层 CHECK 约束（SQLite 支持 `CHECK`）或启动自检。

---

# COR-02 · WASM host 读取先分配、后校验（宿主 OOM）　`P1` `[实测]`

### 位置
[crates/automation-core/src/wasm.rs:148-163](../../crates/automation-core/src/wasm.rs#L148-L163)（`read_guest_str`）

### 问题描述

```rust
let start = ptr.max(0) as usize;
let size = len.max(0) as usize;
let mut buf = vec![0u8; size];        // ← 先按 guest 给的 len 分配宿主堆
mem.read(caller, start, &mut buf)?;   // ← 再做越界校验（已晚）
```
`len` 是 guest 传入的 `i32`，可达 `0x7FFFFFFF`（≈2GB）。`nf.log`/`nf.notify` 等 host 函数都经过此函数。

### 影响

恶意/缺陷插件一行 `nf.log(0, 0x7FFFFFFF)` 即触发宿主侧 ~2GB 分配 → 分配失败则 panic/abort（**非隔离崩溃**，违反"插件崩溃不影响宿主"的沙箱目标）；若机器内存充足则造成长时间卡顿。**关键点**：wasmtime 的 `StoreLimits`（64MB 线性内存）只约束 guest 内存，管不到宿主为拷贝分配的缓冲。

### 解决方案

```rust
fn read_guest_str(caller: &mut Caller<'_, HostCtx<'_>>, ptr: i32, len: i32) -> wasmtime::Result<String> {
    /// 单次 host 调用可读取的 guest 字符串上限（协议层语义上限）
    const MAX_GUEST_STR: usize = 1 << 20;   // 1 MiB

    let mem: Memory = caller.get_export("memory").and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("guest 未导出 memory"))?;

    let start = usize::try_from(ptr).map_err(|_| wasmtime::Error::msg("ptr 为负"))?;
    let size = usize::try_from(len).map_err(|_| wasmtime::Error::msg("len 为负"))?;
    if size > MAX_GUEST_STR {
        return Err(wasmtime::Error::msg(format!("guest 字符串长度 {size} 超过上限 {MAX_GUEST_STR}")));
    }
    let end = start.checked_add(size).ok_or_else(|| wasmtime::Error::msg("ptr+len 溢出"))?;
    if end > mem.data_size(caller) {
        return Err(wasmtime::Error::msg(format!("越界读取 guest 内存: {start}..{end} > {}", mem.data_size(caller))));
    }
    let mut buf = vec![0u8; size];          // 此时 size 已被双重限制
    mem.read(caller, start, &mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 分配次序 | 先 `vec![0; guest_len]`（最大 2GB） | 校验长度上限 + 边界后才分配 |
| 越界读取 | 分配已发生 | 直接 `Err`，零分配 |
| 绕过 `StoreLimits` | 可绕过（宿主侧缓冲） | 不可绕过（宿主自有上限） |
| 失败语义 | 可能 abort | 返回 `wasmtime::Error` → 插件调用失败，宿主正常 |

### 预防措施
1. **host 函数统一契约**：所有 `(ptr, len)` 读取必须经此单一函数；新增 host 函数评审时检查"是否复用 `read_guest_str`"。
2. **宿主侧资源预算显式化**：为插件执行定义"单次调用输入/输出上限 + 单次执行总分配上限"，写进 `docs/impl/07` 的插件契约。
3. **测试**：`assert!(read_guest_str(&mut caller, 0, i32::MAX).is_err())`，以及"越界 ptr"负例。

---

# COR-03 · Vault `VirtualLock` 锁错地址（防换页未生效）　`P1` `[实测]`

### 位置
[crates/vault-core/src/vault.rs:155-156,193-194,378](../../crates/vault-core/src/vault.rs#L155-L156)；[crates/vault-core/src/crypto.rs:70-88](../../crates/vault-core/src/crypto.rs#L70-L88)

### 问题描述

```rust
// vault.rs:155-156
dek.lock_in_memory();          // 对"栈上 dek"内部的 [u8;32] 调 VirtualLock
*inner = Inner::Unlocked { dek };   // 随即把 dek 移动进 Mutex<Inner> —— 内联数组地址改变
```
```rust
// crypto.rs:74-80 —— SecretKey 是内联数组（见 crypto.rs:50 附近定义）
pub fn lock_in_memory(&self) {
    let ptr = self.0.as_ptr() as usize;      // 取当前地址
    if !port.lock(ptr, KEY_LEN) { warn!("VirtualLock 锁页失败") }
}
pub fn unlock_memory(&self) { port.unlock(self.0.as_ptr() as usize, KEY_LEN) }  // 解锁时又是新地址
```
`SecretKey` 内含 `[u8; KEY_LEN]`，**移动即复制**。锁的是移动前地址；真正长期驻留密钥的是移动后地址（未被锁）。`unlock_memory` 又对"从未锁过的地址"解锁（`VirtualUnlock` 失败被忽略）。

### 影响
D-24 声称的"DEK 防换页泄露"**实际未达成**——密钥仍可能被写入页文件（休眠文件/页面文件），在取证或页文件泄露场景下可被提取。同时 `unlock_memory` 的失败被静默忽略，掩盖了不一致。

### 解决方案

**步骤 1：让密钥缓冲区地址稳定（堆分配），从根上消除"移动即换址"**

```rust
// crates/vault-core/src/crypto.rs
pub struct SecretKey(Box<[u8; KEY_LEN]>);      // ← 内联数组改为 Box：移动只搬指针，堆地址稳定

impl SecretKey {
    pub fn new(bytes: &[u8]) -> Self {
        let mut b = Box::new([0u8; KEY_LEN]);
        b.copy_from_slice(bytes);
        Self(b)
    }
    pub fn lock_in_memory(&self) {              // 任意时刻调用都作用于同一块堆内存
        let Some(port) = mem_lock_port() else { return };
        if !port.lock(self.0.as_ptr() as usize, KEY_LEN) {
            tracing::warn!("VirtualLock 锁页失败：DEK 可能驻留页文件（不阻断解锁）");
        }
    }
    pub fn unlock_memory(&self) { if let Some(p) = mem_lock_port() { p.unlock(self.0.as_ptr() as usize, KEY_LEN) } }
}
impl Drop for SecretKey { fn drop(&mut self) { self.0.zeroize(); } }   // Box 内数组仍会 zeroize
```

**步骤 2：调用点顺序改为"先落位、后锁页"，并在 Drop/锁定时配对解锁**

```rust
// vault.rs:155-156 与 :193-194
*inner = Inner::Unlocked { dek };   // 先落位
if let Inner::Unlocked { dek } = &*inner { dek.lock_in_memory(); }   // 再锁（地址已稳定）
```
（Box 化之后顺序已不敏感，但显式顺序更易审阅。）

**步骤 3：为 `unlock_memory` 记录失败**

```rust
if !port.unlock(ptr, KEY_LEN) {
    tracing::warn!("VirtualUnlock 失败：地址可能从未锁定（内存锁状态不一致）");
}
```

**步骤 4：测试**

```rust
#[test]
fn secret_key_lock_address_is_stable_across_moves() {
    let k = SecretKey::new(&[7u8; KEY_LEN]);
    let p1 = k.0.as_ptr() as usize;
    let moved = k;                    // 移动
    assert_eq!(p1, moved.0.as_ptr() as usize, "移动后堆地址必须不变（否则 VirtualLock 失效）");
}
#[test]
fn lock_and_unlock_are_paired() {
    // FakeMemLockPort 记录 (lock/unlock) 调用，断言成对且地址相同
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 密钥存储 | 内联 `[u8;32]`（移动即换址） | `Box<[u8;32]>`（堆地址稳定） |
| 锁页时机 | 移动前（锁错地址） | 任意时刻都锁在真实地址 |
| 解锁 | 对未锁地址解锁、失败忽略 | 地址一致 + 失败告警 |
| 防换页 | **未生效** | 生效（Fake 端口可断言） |

### 预防措施
1. **"敏感的稳定内存"用 Box/Arc 承载**：任何要 `VirtualLock`/裸指针访问的缓冲区都必须堆分配并固定地址；把该规则写进 vault 模块注释与评审清单。
2. **`MemLockPort` 增加"地址记账"**：测试用 Fake 端口断言"锁过的地址 = 解锁的地址"，把内存锁语义纳入自动化验证（当前仅有"失败不阻断"的宽松测试）。
3. **同类排查**：Windows Hello 信封、DPAPI 解包中间缓冲（`COR-12`）、KVM 静态私钥（`StaticSecret` 亦为内联数组，若需锁页需同样处理）。

---

# COR-04 · 编辑器保存非原子写 + 立即删草稿　`P1` `[实测]`

### 位置
[crates/editor-core/src/session.rs:301-316（save）](../../crates/editor-core/src/session.rs#L301-L316)、[:332-342（save_as）](../../crates/editor-core/src/session.rs#L332-L342)

### 问题描述

```rust
// session.rs:311-316
// 写锁已释放再写文件（避免 IO 慢操作持锁）
if let Some(s) = self.lock().get(id) {
    std::fs::write(&s.path, &raw)?;                        // ← 直接截断 + 写，非原子
    let _ = std::fs::remove_file(autosave_path(&s.path));  // ← 成功后立刻删草稿
}
```
`std::fs::write` 先 `create/truncate` 再写。若在写入中途崩溃/断电/磁盘满，**用户源文件已被截断**；而此时 autosave 草稿已被删除（草稿删除发生在 `fs::write` 返回 Ok 之后，看起来安全——但 `write` 的 Ok 只代表缓冲已提交到 OS，崩溃后仍可能只落部分数据；更重要的是**「先截断」本身就是风险窗口**）。全仓其他落盘均为 tmp+rename（`notes-core/canvas.rs:39-41`、`host-core/config.rs:227-231`、`file-core/driver.rs:89-91`、`editor-core/session_store.rs:50-51`），**只有编辑器正文保存是例外**。

另外 `save` 中 `self.lock().get(id)` 是"取路径用了就放"的写法：两次取锁之间会话可能被 `close`/`reload`，导致写入旧路径（TOCTOU）。

### 影响
用户正在编辑的源码/文档在保存瞬间遇到崩溃或磁盘满 → **文件被截断或半写，且草稿已删**，数据不可恢复。

### 解决方案

**步骤 1：抽出 `write_atomic`（同目录 tmp + rename，失败清理 tmp）**

```rust
// crates/editor-core/src/session.rs
/// 原子写：同目录临时文件 → fsync → rename（Windows 上 rename 会覆盖已存在目标）
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), EditorError> {
    let tmp = path.with_extension("nforge-tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;                       // 确保内容落盘再改名（避免 rename 后内容仍在缓存）
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        EditorError::Io(e)
    })
}
```
> `fs::rename` 在 Windows 使用 `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING)`，会覆盖目标（`device.rs::PairStore::persist` 已依赖同一语义）——因此**不要**先 `remove_file`（见 `COR-11` 同类错误）。

**步骤 2：`save` / `save_as` 改用 `write_atomic`，并在成功后才清 dirty/删草稿**

```rust
let (path, raw) = {
    let map = self.lock();
    let s = map.get(id).ok_or_else(|| EditorError::NotFound(id.to_string()))?;
    let unified = normalize_eol(&s.content, s.eol);
    (s.path.clone(), encode(&unified, s.effective_encoding()))
};                                    // ← 一次取锁拿到 path 与内容，避免二次取锁的 TOCTOU
write_atomic(&path, &raw)?;
let _ = std::fs::remove_file(autosave_path(&path));
// 之后才更新 dirty/size/encoding
```
（若必须为长 IO 释放锁，也应把 `path` 与 `raw` 在同一锁内取好，如上方写法。）

**步骤 3：autosave 草稿的保护语义** —— 草稿应在"源文件原子替换成功后"才删除；若 `write_atomic` 失败，草稿**必须保留**（当前已是此语义，改用 `?` 传播即可）。

**步骤 4：测试**

```rust
#[test]
fn save_is_atomic_and_keeps_draft_on_failure() {
    // ① 正常保存：断言文件内容正确且草稿被删
    // ② 注入失败（把 tmp 目标设为只读目录 / 用只读文件占位）：断言原文件内容未被截断、草稿仍在
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 写入方式 | `fs::write`（先截断） | 同目录 tmp + `sync_all` + rename |
| 崩溃窗口 | 源文件可被截断/半写 | 源文件要么是旧内容、要么是新内容（原子） |
| 草稿 | 与写入同寿命（脆弱） | 仅在原子替换成功后删除 |
| TOCTOU | 二次取锁取路径 | 一次取锁取 path+内容 |
| 一致性 | 与全仓其他落盘不一致 | 与其他落盘统一 |

### 预防措施
1. **把"原子写"提升为 `host-core::fs::write_atomic` 公共工具**（项目已收敛 `host-core::util`），全仓统一调用；CI 断言生产代码中 `std::fs::write(` 只出现在该工具与测试内。
2. **"就地覆盖 vs 原子替换"评审项**：任何 `save`/`export`/`watermark`（见 `COR-?` PDF 水印同类问题）都必须走原子替换。
3. **加"保存失败不掉数据"的回归测试**到 CI 常跑套件（这是数据安全红线级用例）。

---

# COR-05 · 设备身份 fail-open 重生覆盖（信任根被摧毁）　`P1` `[实测]`

### 位置
[crates/host-core/src/device.rs:79-99](../../crates/host-core/src/device.rs#L79-L99)、[:134-146](../../crates/host-core/src/device.rs#L134-L146)

### 问题描述

```rust
// device.rs:84-98
if let Ok(bytes) = std::fs::read(&path) {
    if let Ok(id) = deserialize_identity(&bytes, crypto.clone()) { return Ok(id); }
    tracing::warn!("身份文件损坏，重新生成");       // ← "读得到但解不开" 与 "不存在" 被同等对待
}
let id = Self::generate(device_name);
let bytes = serialize_identity(&id, crypto)?;      // crypto=None 时明文落盘
std::fs::write(&path, bytes)?;                     // ← 覆盖原身份
```
"解不开"的典型触发（`:140-141`）：`persisted.protected == true` 但本次启动 `CryptoPort` 未注册 → `deserialize_identity` 返回 `Err` → 被判为"损坏" → 生成新身份并覆盖。

### 影响
- `device_id` 与密钥对**永久改变**：`paired.json` 里所有对端指纹失配 → KVM 配对全部失效、同步信任关系需重建。
- 新身份可能以**未受 DPAPI 保护的明文私钥**落盘（`crypto=None` 分支）——安全等级悄悄降低。
- 与同仓纪律相反：`sys-core/process.rs:106-124` 的保护名单坏盘是**总拒**、`known_hosts` 坏档**拒启**（fail-closed）。

### 解决方案

```rust
// crates/host-core/src/device.rs
pub fn load_or_create(dir: &PathBuf, crypto: Option<Arc<dyn CryptoPort>>) -> Result<Self, ModuleError> {
    let path = dir.join("identity.json");
    match std::fs::read(&path) {
        Ok(bytes) => match deserialize_identity(&bytes, crypto.clone()) {
            Ok(id) => { tracing::info!(device_id = %id.device_id, "设备身份已加载"); Ok(id) }
            Err(e) => {
                // 存在但不可解：fail-closed。留证、报错、绝不覆盖（避免摧毁既有配对）
                let quarantine = path.with_extension("json.corrupt");
                let _ = std::fs::rename(&path, &quarantine);
                Err(ModuleError::Init(format!(
                    "设备身份无法解析（{e}）；原文件已保留为 {}。若确认要重建身份，请手动删除该文件后重启（注意：将导致既有配对失效）",
                    quarantine.display()
                )))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 仅"确实不存在"才生成；原子写；无 CryptoPort 时明确告知风险
            if crypto.is_none() {
                tracing::warn!("CryptoPort 未注册：设备私钥将以未加密形式落盘");
            }
            let id = Self::generate(std::env::var("COMPUTERNAME").unwrap_or_else(|_| "NexusForge".into()));
            let bytes = serialize_identity(&id, crypto)?;
            std::fs::create_dir_all(dir).map_err(|e| ModuleError::Init(e.to_string()))?;
            write_atomic(&path, &bytes)?;
            tracing::info!(device_id = %id.device_id, "设备身份已创建");
            Ok(id)
        }
        Err(e) => Err(ModuleError::Init(format!("读取身份文件失败: {e}"))),
    }
}
```
配套：`ModuleError::Init` 经 IPC 映射为带 hint 的 `AppError`，提示用户"若为 CryptoPort 未就绪导致，请重启应用；请勿直接删除身份文件"。

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 文件不存在 | 生成 | 生成（不变） |
| 文件存在但不可解 | **覆盖 + 生成新身份** | 隔离留证 + 报错，拒绝覆盖 |
| CryptoPort 缺失 | 静默降级为明文身份 | 明确警告（若需生成）/ 直接报错（若已有加密身份） |
| 既有配对 | 全部失效且无告警 | 保留；用户显式操作才重建 |
| 落盘 | `fs::write` | 原子写 |

### 预防措施
1. **"信任根文件"统一 fail-closed**：把 `identity.json`/`paired.json`/`known_hosts`/`process_guard.json` 列入清单，全部要求"不可解 → 拒绝启动相关功能 + 留证 + 可操作提示"，并各配负例测试。
2. **区分"缺失"与"损坏"的代码范式**：`match read { NotFound => create, Ok(bytes) => parse_or_fail, Err(e) => fail }` 应成为标准写法；把 `if let Ok(bytes) = read` 这种**吞掉非 NotFound 错误**的写法列入 code review 禁用模式。
3. **身份文件加固**：同时记录"公钥指纹"到独立文件（不被覆盖），启动时交叉校验并告警不一致；对私钥落盘校验"是否受保护"（`protected` 字段与实际一致）。

---

# COR-06 · 模块重启后监听/线程不再恢复　`P1` `[实测]`

### 位置
- [crates/sync-core/src/module.rs:1356-1367](../../crates/sync-core/src/module.rs#L1356-L1367)、[:1401-1407](../../crates/sync-core/src/module.rs#L1401-L1407)
- [crates/sys-core/src/module.rs:145-152](../../crates/sys-core/src/module.rs#L145-L152)（`start_sampler`）、[:174](../../crates/sys-core/src/module.rs#L174)
- [crates/desktop-core/src/module.rs:157-161](../../crates/desktop-core/src/module.rs#L157-L161)（`start_remind_loop`）、[:199](../../crates/desktop-core/src/module.rs#L199)

### 问题描述

`stop()` 置 `cancel = true`，但 `start()` **不复位**：
```rust
// sync-core stop():1402
self.cancel.store(true, Ordering::SeqCst);
// sync-core start():1358 —— 直接克隆（仍是 true）
let cancel = self.cancel.clone();
tokio::spawn(async move { SyncModule::accept_loop(ctx_listen, port, cancel, ...).await });
// accept_loop:1292 —— bind 成功后第一轮即 break
loop { if cancel.load(Ordering::SeqCst) { break; } ... }
```
（讽刺的是 `start()` 的注释 [:1359-1360](../../crates/sync-core/src/module.rs#L1359-L1360) 明确说"每次启动先把两枚事实清零"——只清了 `listening` 与 `bind_error`，**漏了 `cancel`**。）

sys-core / desktop-core 同形，且是"句柄不清空"变体：
```rust
// sys-core:146-149
let already = self.sample_cancel.swap(false, SeqCst);
if already && self.sample_thread.read().is_some() { return; }   // stop 时线程已退出，但句柄仍 Some → 永久早退
```

### 影响
`host_module_restart`（`src-tauri/src/commands/mod.rs:82` → registry.restart）是 **UI 可达**路径（状态栏红点/模块面板"重启"）：
- **sync**：重启后监听永久失效，`listening=false`——只能主动发起同步，无法被对端同步（用户以为已恢复）。
- **sys**：`sys.metrics` 事件流静默中断（系统监控面板不再更新）。
- **desktop**：`desktop.remind_due` 不再产生（提醒功能静默失效）。
- 三处 `stop` 也都不 join 线程（句柄悬挂/线程泄漏）。

### 解决方案

**步骤 1：`start` 首行复位取消信号**

```rust
// sync-core/module.rs start()
self.cancel.store(false, Ordering::SeqCst);      // ← 新增：必须在 clone 之前
let cancel = self.cancel.clone();
self.listening.store(false, Ordering::SeqCst);
*self.bind_error.write() = None;
```

**步骤 2：句柄改为"可判定存活"，stop 时清理**

```rust
// sys-core/module.rs start_sampler()
let alive = self.sample_thread.read().as_ref().is_some_and(|h| !h.is_finished());
if alive { return; }                       // 线程仍在跑 → 不重复启动
*self.sample_thread.write() = None;        // 清掉已退出线程的句柄

// sys-core/module.rs stop()
self.sample_cancel.store(true, Ordering::SeqCst);
if let Some(h) = self.sample_thread.write().take() {
    // 不在同步上下文里 join（可能阻塞）；交给一个短命线程回收
    std::thread::spawn(move || { let _ = h.join(); });
}
```
desktop-core 同改（`remind_cancel` 复位 + `remind_thread` 判定 `is_finished()`/`take()`）。

**步骤 3：加"重启后仍工作"的回归测试（当前 `module_lifecycle.rs` 未覆盖此语义）**

```rust
#[tokio::test]
async fn restart_restores_sync_listening() {
    let m = build_sync_module();
    m.start().await.unwrap();
    assert!(m.listening.load(SeqCst));
    m.stop().unwrap();
    m.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(m.listening.load(SeqCst), "重启后监听必须恢复");
}
#[test]
fn restart_sampler_publishes_metrics_again() { /* 断言 sys.metrics 事件在重启后仍到达 */ }
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| `cancel` 生命周期 | stop 置 true 后永不复位 | start 复位为 false |
| sync 监听 | 重启后永久失效 | 重启后恢复 |
| sys/desktop 线程 | 句柄永久 Some → 早退，事件静默中断 | 判定存活/清空句柄 → 正常重启 |
| 线程句柄 | 悬挂、不 join | take + 后台 join 回收 |
| 测试 | 无"重启后仍工作"用例 | 新增 3 条（含事件流断言） |

### 预防措施
1. **"可重启"作为模块契约并测试化**：`DESIGN.md §8.2` 已要求可重启，应把 `start→stop→start` 的行为断言纳入每个模块的测试模板（现仅 `module_lifecycle.rs` 覆盖 4 个模块的部分语义）。
2. **取消信号用单一"代际令牌"而非裸 `AtomicBool`**：`tokio_util::sync::CancellationToken`（`cancel()`/`child_token()`）天然支持"新一轮新建 token"，避免"忘记复位"这类 bug——建议作为标准工具在 `host-core` 收敛。
3. **句柄与取消信号成对管理**：写入即记录、停止即取出并回收；`CLAUDE/CODE_REVIEW` 清单里加一句"stop 是否 join/abort 了它 start 的东西"。

---

# COR-07 · sync 变更订阅任务不可退出 + 重启叠加　`P1` `[实测]`

### 位置
[crates/sync-core/src/module.rs:1368-1396](../../crates/sync-core/src/module.rs#L1368-L1396)（start 内 spawn），[:1401-1407](../../crates/sync-core/src/module.rs#L1401-L1407)（stop）

### 问题描述

```rust
// :1371-1394
if let Ok(mut rx) = bus.subscribe("notes.changed") {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => { ... record_change_event(&ctx2, &event) ... }
                Err(Lagged(_)) => continue,
                Err(_) => break,          // ← 仅"发送端全部关闭"才退出；总线长期持有 Sender
            }
        }
    });                                   // ← JoinHandle 未保存
}
```
`stop()` 只置 `cancel`，既不 `abort` 也不 `join` 该任务。每次 `start()` 再 spawn 一个 → 同一 `notes.changed` 事件被 N 个订阅者各处理一次。

### 影响
- **任务泄漏**：每次重启多一个常驻任务（与 `COR-06` 组合，重启越多次越严重）。
- **重复记账**：`record_change_event` 用 `Uuid::now_v7()` 生成 `op_id`（[:881-888](../../crates/sync-core/src/module.rs#L881-L888)），`INSERT OR IGNORE` **无法去重**（两条 op 的 id 与 ts 都不同）→ 对端收到**重复变更**，可能触发重复应用、冲突误判、同步流量放大。

### 解决方案

**步骤 1：保存 `JoinHandle` 并在 stop 时 abort（同时保留 cancel 检查作为二道保险）**

```rust
// 结构体新增字段
struct SyncModule { ..., sub_task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>> }

// start()：先清理上一轮
if let Some(h) = self.sub_task.lock().take() { h.abort(); }
...
let handle = tokio::spawn(async move {
    loop {
        if cancel.load(Ordering::SeqCst) { break; }        // 二道保险
        match rx.recv().await {
            Ok(event) => { ... }
            Err(Lagged(_)) => continue,
            Err(_) => break,
        }
    }
});
*self.sub_task.lock() = Some(handle);

// stop()：abort 该任务
if let Some(h) = self.sub_task.lock().take() { h.abort(); }
```

**步骤 2（可选加固）：`op_id` 改用"内容哈希 + 源事件 id"**，使重复订阅也不会产生重复 op：

```rust
fn op_id_for(event: &Event) -> String {
    // 同一源事件（含其 id/时间戳）在任何订阅者下都得到同一 op_id
    let mut h = Sha256::new();
    h.update(event.id.as_bytes());
    h.update(event.payload.to_string().as_bytes());
    hex(h.finalize())
}
```
（若 `Event` 无 id 字段，则用 `(topic, entity_id, ts, payload_hash)` 组合。）

**步骤 3：测试**

```rust
#[tokio::test]
async fn restart_does_not_duplicate_subscription() {
    let m = build_sync_module();
    m.start().await.unwrap(); m.stop().unwrap(); m.start().await.unwrap();
    publish_notes_changed_once().await;
    // 断言 oplog 中该变更只有 1 条（非 2 条）
}
#[tokio::test]
async fn subscription_task_exits_on_stop() { /* 断言 stop 后任务数不再增长（可暴露内部计数用于测试） */ }
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 任务生命周期 | 仅发送端关闭才退出（实际永不） | stop 时 `abort()`（+ cancel 检查） |
| 重启叠加 | 每次 start 多 1 个订阅者 | 先 abort 旧任务再启动 |
| 重复变更 | 同一事件写 2 条 op | 仅 1 条（且 op_id 内容化后天然去重） |
| 测试 | 无 | 新增 2 条 |

### 预防措施
1. **"每个 spawn 都必须有归属"**：在 `CONTRIBUTING.md`/评审清单写明"`tokio::spawn`/`thread::spawn` 的句柄必须存入结构体，并在 stop/Drop 中收回（abort/join/取消）"。可加 clippy 自定义 lint 或源码扫描断言（`tokio::spawn(` 出现处必须同行/邻近有赋值给字段）。
2. **事件订阅统一封装**：在 `host-core` 提供 `Subscription` 类型（Drop 即取消），替代裸 `tokio::spawn(loop)`；`events.rs:258` 已有未启用的 `Subscription` 别名，正好补完。
3. **幂等标识**：所有"由事件派生并持久化"的记录，主键应可由事件内容稳定推导（避免"重复消费产生新记录"）。

---

# COR-08 · 远端变更应用失败仍推进游标（永久丢变更）　`P1` `[实测]`

### 位置
[crates/sync-core/src/module.rs:510-548](../../crates/sync-core/src/module.rs#L510-548)

### 问题描述

```rust
for op in ops {
    let applier = self.applier_for(&op.entity)?;
    max_ts = max_ts.max(op.ts);                     // ← 先记账（无论成败）
    match SyncEngine::apply_remote(...) {
        ...
        Err(e) => tracing::warn!(op_id = %op.op_id, error = %e, "远端变更应用失败（跳过）"),  // ← 只 warn
    }
}
if max_ts > 0 { self.log.set_cursor(peer_key, max_ts)?; }    // ← 整批推进游标
```
注释（[:505-509](../../crates/sync-core/src/module.rs#L505-L509)）表明作者已意识到"不得谎报进度"，但**只在 `applier_for` 失败臂收口**（返回 `Err`），`apply_remote` 失败仍被降级为 warn。

### 影响
磁盘满/文件被占用/权限不足导致 `apply_remote` 失败（例如 `NotesApplier::apply_upsert` → `lib.write/create` 失败，`src-tauri/src/state.rs:523`）→ 该 op 永远不会再拉取（游标已越过），而 `sync_run` 与面板按协议记账显示"已同步 X 条"。用户看到"同步成功"，实际数据缺失，且**无任何提示**。

### 解决方案

**步骤 1：游标只推进到"最后一个成功应用的 op"，失败即停批**

```rust
let mut last_ok_ts = 0i64;
let mut failed: Vec<(String, String)> = Vec::new();
for op in ops {
    let applier = self.applier_for(&op.entity)?;
    match SyncEngine::apply_remote(applier, &op) {
        Ok(_) => last_ok_ts = last_ok_ts.max(op.ts),
        Err(e) => {
            tracing::warn!(op_id = %op.op_id, error = %e, "远端变更应用失败：停止推进游标，稍后重试");
            failed.push((op.op_id.clone(), e.to_string()));
            break;                       // ← 变更有序，必须停下（不能跳过后继续）
        }
    }
}
if last_ok_ts > 0 { self.log.set_cursor(peer_key, last_ok_ts)?; }
if !failed.is_empty() {
    // 记账为"部分失败"，让面板与 sync_run 如实显示
    return Err(AppError::module(
        "SYNC_APPLY_003",
        format!("{} 条远端变更应用失败（已保留游标，下次同步将重试）", failed.len()),
        Some("检查目标文件是否被占用/磁盘空间/权限后重试同步".into()),
    ));
}
```

**步骤 2：失败具名化与去重告警** —— 把失败 op 的 `entity_id` 记入 `sync_run.error` 与冲突面板（"未应用"分类），让用户能看到"哪几条没同步上"。

**步骤 3：测试（当前完全缺失）**

```rust
#[tokio::test]
async fn cursor_does_not_advance_past_failed_apply() {
    // 注入一个必然失败的 applier（目标路径只读）→ 断言：
    // ① 返回 Err；② 游标仍停在失败 op 之前的 ts；③ 下次同步会再次拉到该 op
}
#[tokio::test]
async fn successful_batch_advances_cursor_to_last_ts() { /* 正向对照 */ }
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 失败处理 | warn + 继续，游标照推 | 停止本批，游标停在最后成功处 |
| 数据一致性 | 变更永久丢失且显示"已同步" | 变更保留待重试，界面如实显示失败 |
| 可观测性 | 仅日志 | 返回 `AppError`（含 hint）+ 失败清单 |
| 测试 | 无 | 2 条（负例 + 正向对照） |

### 预防措施
1. **"进度/游标只在成功后推进"作为不变式**，写入同步模块文档；任何 `set_cursor` 调用点都必须能回答"此处是否可能跳过失败项"。
2. **统一"失败不可静默"纪律**：项目已有 `tracing::warn!` 习惯，但 warn 对用户不可见；应确立"数据类失败必须回报到 UI（`AppError` + hint 或降级状态）"的规则（`DESIGN.md §8.1` 已在错误码层面要求，需在数据面落实）。
3. **建立"投毒/失败注入"测试**：为 applier/写盘/网络各注入一次失败，断言"不丢数据 + 游标正确 + 用户可见"，纳入 CI 常跑。

---

# COR-09 · 非 UTF-8 笔记被 lossy 解码后 UTF-8 回写（不可逆损坏）　`P1` `[实测]`

### 位置
[crates/notes-core/src/library.rs:77-83](../../crates/notes-core/src/library.rs#L77-L83)（`read_text`）、[:390-402](../../crates/notes-core/src/library.rs#L390-L402)（`rename` 内的链接改写）

### 问题描述

```rust
// :77-83
fn read_text(&self, rel: &str) -> Result<String> {
    let bytes = self.driver.read_file(&self.disk(rel))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())     // ← 非法 UTF-8 → U+FFFD，且不记录原编码
}

// :390-402（重命名时改写 [[链接]]）
let Ok(content) = self.read_text(src) else { continue };
let next = rewrite_links(&content, &old_stem, &new_stem);
if next != content {
    if let Err(e) = self.write_text(src, &next) { warn!(...) }   // ← 以 UTF-8 覆盖回原文件
}
```
GBK/GB18030/Shift-JIS 等笔记只要**含 `[[旧名]]` 且被重命名**，就会被 lossy 解码（中文全变 `U+FFFD`）后以 UTF-8 写回 → 原始字节**永久丢失**。

### 影响
中文/日文笔记在批量重命名（笔记域的高频操作）后出现大面积 ``，且不可逆（`write_text` 覆盖原文件）。这是"看起来可用、实际毁数据"的典型。

### 解决方案

**注意模块隔离约束**：`notes-core` **不能**直接依赖 `editor-core`（模块间禁止直接依赖，见硬约束）。因此需要把编码检测下沉到 `host-core`。

**步骤 1：在 `host-core` 提供共享的"编码检测 + 解码/编码"工具**

```rust
// crates/host-core/src/text.rs（新增，供 editor-core 与 notes-core 共用）
pub struct DecodedText { pub text: String, pub encoding: &'static encoding_rs::Encoding, pub had_bom: bool, pub lossy: bool }

/// BOM → 严格 UTF-8 → chardetng 检测；lossy 标记"是否发生过替换"
pub fn detect_and_decode(bytes: &[u8]) -> DecodedText {
    use encoding_rs::{UTF_8, UTF_8_BOM};
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let (t, _, _) = UTF_8_BOM.decode(bytes);       // 去 BOM
        return DecodedText { text: t.into_owned(), encoding: UTF_8, had_bom: true, lossy: false };
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return DecodedText { text: s.to_string(), encoding: UTF_8, had_bom: false, lossy: false };
    }
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
    let (t, _, lossy) = enc.decode(bytes);
    DecodedText { text: t.into_owned(), encoding: enc, had_bom: false, lossy }
}

/// 按检测到的编码编码回字节（含 BOM 还原）
pub fn encode_with(d: &DecodedText, text: &str) -> Vec<u8> {
    let (out, _, _) = d.encoding.encode(text);
    if d.had_bom { let mut v = vec![0xEF, 0xBB, 0xBF]; v.extend_from_slice(&out); v } else { out.into_owned() }
}
```
（`encoding_rs`/`chardetng` 已在 workspace 依赖中；`editor-core` 改为复用该工具以避免两套实现漂移。）

**步骤 2：`notes-core` 的读取/回写改为"保留原编码"**

```rust
// library.rs
fn read_text_enc(&self, rel: &str) -> Result<DecodedText> {
    let bytes = self.driver.read_file(&self.disk(rel)?)?;
    Ok(host_core::text::detect_and_decode(&bytes))
}

// rename() 链接改写
let Ok(decoded) = self.read_text_enc(src) else { continue };
if decoded.lossy {
    tracing::warn!(path = %src, "文件含非当前编码字节，跳过链接改写以避免损坏");
    continue;                                  // ← 最低限度：绝不以 lossy 结果回写
}
let next = rewrite_links(&decoded.text, &old_stem, &new_stem);
if next != decoded.text {
    let bytes = host_core::text::encode_with(&decoded, &next);   // 按原编码写回
    driver.write_file(&self.disk(src)?, &bytes)?;                 // 原子写（tmp+rename）
}
```

**步骤 3：`write_text`（新建/用户编辑）保持 UTF-8 语义**，但"改写已有文件"一律按原编码；接口上区分 `write_new()` 与 `rewrite_preserving_encoding()`。

**步骤 4：测试（含真实编码夹具）**

```rust
#[test]
fn rename_rewrites_links_preserving_gbk_files() {
    // 造一个 GBK 编码、含 "[[旧名]]" 的文件（字节级写入），执行重命名
    // 断言：文件仍是 GBK 编码；解码后中文正确（无 U+FFFD）；链接已更新
}
#[test]
fn rename_skips_lossy_decode() {
    // 造一个非法 UTF-8 的混合文件 → 断言被跳过且字节完全不变
}
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 读取 | `from_utf8_lossy`（丢信息） | 检测链（BOM→严格 UTF-8→chardetng）+ `lossy` 标记 |
| 回写 | 恒 UTF-8 覆盖 | 按原编码 + BOM 还原 |
| 非 UTF-8 笔记 | 中文变 `` 且不可逆 | 内容与编码均保留 |
| 不可解码文件 | 静默损坏 | 跳过改写 + `warn` 点名 |
| 实现位置 | notes 与 editor 各一套（潜在漂移） | `host-core::text` 单点 |

### 预防措施
1. **"文本文件的读写必须编码闭环"**：任何"读文本 → 处理 → 写回同一文件"的链路都要携带编码信息（本项目已有 editor 的检测实现，应上收共享）。
2. **禁止 `from_utf8_lossy` 用于"将要回写的文件"**：可写进 review 清单；仅允许用于日志/展示。
3. **编码夹具测试**：在 `tests/fixtures` 放 GBK/Big5/Shift-JIS/带 BOM 的小文件，作为笔记与编辑器的共享回归夹具。
4. **批量操作先做"干跑"**：重命名/批量替换类操作应默认预览 + 报告"将跳过的文件及原因"，避免大规模静默损坏。

---

# COR-10 · KVM 文件传输：`set_len` 与块偏移未校验　`P2` `[实测]`

**位置**：[transfer.rs:346-372](../../crates/kvm-core/src/transfer.rs#L346-L372)（`on_meta`）、[:377-405](../../crates/kvm-core/src/transfer.rs#L377-L405)（`on_chunk`）

**问题**：
```rust
file.set_len(meta.size)...;                                   // size 由对端声明，无上限
let offset = index * incoming.meta.chunk_size as u64;         // index 未校验 < total_chunks
```
`index` 来自块头、`choft_size`/`size`/`total_chunks` 来自 `FileMeta`，均为**已配对对端可控**。

**影响**：声明 TB 级 `size` → 稀疏文件耗尽磁盘配额；`index * chunk_size` 在 debug 构建溢出 **panic**（DoS），release 下回绕 → `seek` 到错误偏移覆写（数据破坏）。

**解决方案**：
```rust
// on_meta：上限校验
const MAX_TRANSFER_BYTES: u64 = 8 * 1024 * 1024 * 1024;   // 8 GiB，产品口径
const MAX_CHUNK: u32 = 1 << 20;                            // 1 MiB
if meta.size > MAX_TRANSFER_BYTES { return Err(kvm_err("KVM_TRANSFER_020", "文件超过接收上限")); }
if meta.chunk_size == 0 || meta.chunk_size > MAX_CHUNK { return Err(kvm_err("KVM_TRANSFER_021", "块大小非法")); }
let expect_chunks = meta.size.div_ceil(meta.chunk_size as u64);
if meta.total_chunks as u64 != expect_chunks { return Err(kvm_err("KVM_TRANSFER_022", "块总数与大小不符")); }

// on_chunk：索引与偏移校验
if (index as u64) >= incoming.meta.total_chunks { return Err(kvm_err("KVM_TRANSFER_023", "块索引越界")); }
let offset = (index as u64).checked_mul(incoming.meta.chunk_size as u64)
    .ok_or_else(|| kvm_err("KVM_TRANSFER_024", "块偏移溢出"))?;
if offset + data.len() as u64 > incoming.meta.size { return Err(kvm_err("KVM_TRANSFER_025", "块写到文件尾之后")); }
```

**前后对比**：修复前可 DoS/错位写；修复后所有元数据交叉校验（一致性与上限），越界即拒。

**预防措施**：建立"对端可控字段校验表"（size/chunk_size/total_chunks/index/name/entity_id…），每个字段列出上限与一致性关系 + 负例测试；`transfer.rs` 的 receive 逻辑应以 `FileMeta::validate()` 单点入口校验。

---

# COR-11 · Vault `persist_header` 先删后 rename（非原子窗口）　`P2` `[实测]`

**位置**：[vault.rs:449-451](../../crates/vault-core/src/vault.rs#L449-L451)
```rust
// Windows rename 不覆盖已存在目标
let _ = std::fs::remove_file(&self.meta_path);
std::fs::rename(&tmp, &self.meta_path)...
```
**问题**：注释与 Rust 事实不符——`std::fs::rename` 在 Windows 用 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，**会覆盖**（同仓 `device.rs::PairStore::persist` 直接 rename 且工作正常）。多出的 `remove_file` 制造"删旧→写新"窗口。

**影响**：此间崩溃/断电 → `vault.meta.json`（内含包裹 DEK 的信封）丢失 → **保险库永久不可解锁**（不可逆）。

**解决方案**：删除 `remove_file`，直接 `std::fs::rename(&tmp, &self.meta_path)`；`tmp` 与目标同目录、写入后 `sync_all()`（见 `COR-04` 的 `write_atomic`）。

**前后对比**：修复前有不可逆数据丢失窗口；修复后原子替换，崩溃时保持旧文件完整。

**预防措施**：把 `write_atomic`（`COR-04` 建立）作为全仓唯一落盘入口；**禁止**在 rename 前 `remove_file`，并加入 review 禁用模式清单（"Windows rename 不覆盖"是常见错误认知）。

---

# COR-12 · Vault：DPAPI 解出的明文 DEK 中间缓冲未 `zeroize`　`P2` `[走查]`

**位置**：[vault.rs:354-367](../../crates/vault-core/src/vault.rs#L354-L367)（`hello_unlock`）、[crypto.rs:303-315](../../crates/vault-core/src/crypto.rs#L303-L315)（`unwrap_dek`）

**问题**：`let dek_bytes = crypto_port.unprotect(&wrapped)?;` 得到 `Vec<u8>` 明文 DEK，拷进 `SecretKey` 后原缓冲随作用域 drop（**未 zeroize**）。

**影响**：明文密钥残留在已释放堆内存，可能被后续分配复用/转储获取，破坏 vault 的 zeroize 纪律。

**解决方案**：
```rust
use zeroize::Zeroizing;
let dek_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(crypto_port.unprotect(&wrapped)?);
let mut key = [0u8; KEY_LEN];
key.copy_from_slice(&dek_bytes);
let dek = SecretKey::new(&key);      // 或 SecretKey 直接 from Zeroizing
// key 本身也应 zeroize（或用 Zeroizing<[u8;32]>）
```
`unwrap_dek` 内 `open_raw` 的返回同样用 `Zeroizing` 包裹；`SecretKey::new` 内部先清零临时栈数组。

**前后对比**：修复前明文密钥残留在释放内存；修复后中间缓冲确定清零（`Zeroizing` 在 drop 时清零并防编译器优化掉）。

**预防措施**：vault 模块建立"明文密钥生命周期清单"（产生点 → 传递 → 清零点），每个中间变量必须用 `Zeroizing`/`SecretKey` 承载；评审时 grep `Vec<u8>` 与 `[u8; ` 的密钥路径。

---

# COR-13 · 自动化计划时间：应用内用 UTC、系统计划任务用本地时　`P2` `[走查]`

**位置**：[automation-core/src/module.rs:454-464](../../crates/automation-core/src/module.rs#L454-L464)（`current_hhmm_date`，注释称本地时区）、[:303-307](../../crates/automation-core/src/module.rs#L303-L307)（`sync_task` 交给 Windows 计划任务）

**问题**：`now_ms / 1000` 直接做 `div_euclid(86400)` 得 UTC 的 HH:MM（**未加本地偏移**），而 Windows 计划任务按**本地时间**解释同一 `HH:MM`。

**影响**：中国时区下应用内触发与系统级触发相差 8 小时；两条路径同时存在 → 规则重复触发或错时触发（用户设定"每天 9:00"实际在 17:00 与 9:00 各跑一次）。

**解决方案**：
```rust
/// 本地时区的 (HH, MM) —— 与 TaskSchdPort 同一时基
fn current_hhmm_date_local() -> (u8, u8, i32) {
    let now = chrono::Local::now();
    use chrono::Timelike;
    (now.hour() as u8, now.minute() as u8, now.ordinal() as i32)
}
```
并把 `sync_task` 的传参注明"本地时区"，在 `DECISIONS.md` 记录"计划时间统一本地时区（含夏令时由 OS 处理）"。

**前后对比**：修复前 UTC 与本地混用；修复后两条触发路径同基准。

**预防措施**："时间语义必须显式标注时区"——凡涉及 `HH:MM`/日期窗口/静默时段（`quiet_period_ms` 等）的字段，类型或命名中标注 Local/Utc，并加"跨时区/夏令时"单测（`chrono` 可注入固定时区做纯函数测试）。

---

# COR-14 · 全局热键 `unregister` 不注销 OS（改键后旧键仍生效 + 泄漏）　`P2` `[走查]`

**位置**：[host-core/src/hotkey.rs:114-132](../../crates/host-core/src/hotkey.rs#L114-L132)（`register` 只插 `os_map`）、[:151-157](../../crates/host-core/src/hotkey.rs#L151-L157)（`unregister` 只清内存映射）。`HotkeyWinPort::unregister` 已实现（`ports.rs:479`、`win-integration/src/hotkey.rs:224`）但**全仓无调用点**。

**问题**：
```rust
pub fn unregister(&self, binding_id: &str) -> Result<(), AppError> {
    let combo = self.by_id.write().remove(binding_id);
    if let Some(combo) = combo { self.by_combo.write().remove(&combo); }
    Ok(())                       // ← 从不调用 win.unregister(os_id)，也不清 os_map
}
```
**影响**：`owner_of` 已查不到旧组合，但 Windows 侧 `RegisterHotKey` 仍在，分发器（[:60-64](../../crates/host-core/src/hotkey.rs#L60-L64) 读 `os_map`）仍 `fire()` 旧闭包 → **旧快捷键在系统层继续触发同一动作**（内存态与 OS 态不一致）；`os_map` 与 `os_id` 随每次注册单调增长 → 闭包与 id 泄漏（长时间运行/频繁改键会累积）。

**解决方案**：
```rust
// register：把 os_id 与闭包一起记入 by_id
struct Owner { os_id: i32, on_fire: Arc<dyn Fn() + Send + Sync> }
self.by_id.write().insert(binding_id.to_string(), Owner { os_id, on_fire: on_fire.clone() });
self.os_map.write().insert(os_id, on_fire);

// unregister：真正注销
pub fn unregister(&self, binding_id: &str) -> Result<(), AppError> {
    let Some(owner) = self.by_id.write().remove(binding_id) else { return Ok(()) };
    if let Some(combo) = self.combo_of(binding_id) { self.by_combo.write().remove(&combo); }
    self.os_map.write().remove(&owner.os_id);
    self.win.unregister(owner.os_id)?;      // ← 必须调用端口
    Ok(())
}
```
模块 `stop()` 时注销本模块注册的全部 bindings（`bindings()` 遍历 + unregister）。

**前后对比**：修复前"改键后旧键仍可用 + 泄漏"；修复后 OS 态与内存态一致，stop 时干净注销。

**预防措施**："双状态（内存 + OS/外部资源）必须成对维护"——注册/分配类 API（热键、计划任务、服务句柄、文件监听）都要有 `register/unregister` 配对测试：注册→注销→断言外部状态已释放（可用 Fake 端口断言调用序列）。

---

# COR-15 · 笔记切换/新建丢弃未保存内容　`P2` `[实测]`

**位置**：[notes/NotesPanel.tsx:338-353](../../src/modules/notes/NotesPanel.tsx#L338-L353)（`openNote`）、[:884](../../src/modules/notes/NotesPanel.tsx#L884)（列表点击）、[:495-508](../../src/modules/notes/NotesPanel.tsx#L495-L508)（`createNote`）

**问题**：`dirty` 状态存在，但切换/新建前不做校验（EditorPanel 关闭脏缓冲有 `confirmAction`，[:406-416](../../src/modules/editor/EditorPanel.tsx#L406-L416)）。

**影响**：改完未保存点另一篇 → 编辑内容**静默丢失**（无确认、无草稿）。

**解决方案**：
```tsx
const guardDirty = useCallback(async (): Promise<boolean> => {
  if (!dirty) return true;
  return confirmAction({
    title: "放弃未保存的修改？",
    message: `「${active ?? "当前笔记"}」的修改尚未保存，继续将丢弃这些修改。`,
    confirmLabel: "放弃并继续", danger: true,
  });
}, [dirty, active]);

const openNote = useCallback(async (path: string) => {
  if (!(await guardDirty())) return;
  const r = await notesRead(path); setActive(path); setContent(r.content); setDirty(false);
}, [guardDirty]);
// createNote 同改（或先自动保存：更优，dirty 时先 saveNote() 再切换）
```
更友好的替代：`dirty` 时**自动保存**（笔记为纯文本、保存开销小），仅在保存失败时再弹确认。

**前后对比**：修复前静默丢弃；修复后确认或自动保存。

**预防措施**：把"有未保存状态时的离开动作"作为**通用表单约束**：`dirty` 状态与"离开守卫"绑定（可抽 `useDirtyGuard(dirty, onLeave)` hook），并在所有编辑型面板（notes、editor、canvas、vault 条目编辑）统一使用。

---

# COR-16 · 目录切换/剪贴板搜索缺请求序号守卫（旧响应覆盖新状态）　`P2` `[实测]`

**位置**：[file/FilePanel.tsx:233-249](../../src/modules/file/FilePanel.tsx#L233-L249)（`loadDir`）、[clipboard/panels/HistorySection.tsx:368-394](../../src/modules/clipboard/panels/HistorySection.tsx#L368-L394)（`load` + `useEffect`）

**问题**：
```tsx
// HistorySection：load 依赖 queryText → 每击键都 new 一次 → effect 每击键发 2 条 IPC
const res = await clipboardSearch(clipSearchParams(queryText, group, p, 50, typeChip));
setEntries((prev) => (append ? [...prev, ...res.items] : res.items));   // ← 无序号守卫
```
同仓已有正确范式：`SearchSection`（`searchSeq`）、`LauncherWindow.tsx:77-97`（`seq`）。

**影响**：① 慢响应覆盖快响应 → 列表与输入框/`cwd` 不一致（P0 主列表的"错位结果"）；② 每字符 2 次 IPC + FTS 查询（性能）。

**解决方案**：
```tsx
const seqRef = useRef(0);
const load = useCallback(async (p: number, append: boolean) => {
  const seq = ++seqRef.current;
  const res = await clipboardSearch(clipSearchParams(queryText, group, p, 50, typeChip));
  if (seq !== seqRef.current) return;              // ← 丢弃过期响应
  setEntries((prev) => (append ? [...prev, ...res.items] : res.items));
  setHasMore(res.has_more); setPage(p);
}, [queryText, group, typeChip]);

// 并在输入侧加防抖（250ms），让"逐字输入"只发一次查询
const debouncedQuery = useDebounced(queryText, 250);
useEffect(() => { void load(0, false); refreshCounts(); }, [debouncedQuery, group, typeChip]);
```
FilePanel 同改（`dirSeq`）。

**前后对比**：修复前乱序覆盖 + 每击键 2 IPC；修复后结果恒对应最新输入，IPC 次数降为 1/N。

**预防措施**：把 `seq/AbortController` 守卫作为**所有"用户输入驱动 + 异步返回"组件的强制模式**：在 `src/components` 提供 `useSequencedQuery` hook（内部持 seq + 防抖 + 卸载取消），禁止裸 `await` 后直接 `setState`；可加 ESLint 提示或 review 清单项。

---

# COR-17 · 3 处事件监听缺 `cancelled` 守卫（监听泄漏 + 重复处理）　`P2` `[实测]`

**位置**：[HistorySection.tsx:430-432](../../src/modules/clipboard/panels/HistorySection.tsx#L430-L432)、[clipboard/panels/SettingsSection.tsx:178-180](../../src/modules/clipboard/panels/SettingsSection.tsx#L178-L180)、[vault/VaultPanel.tsx:404-406](../../src/modules/vault/VaultPanel.tsx#L404-L406)

**问题**：`listen().then(u => { unlisten = u })` 无守卫；其余 15 处监听（`MainWorkbench.tsx:148-154` 等）都有 `cancelled/disposed` 模式。卸载早于 `listen()` resolve 时，迟到监听器被注册却永不注销。

**影响**：监听泄漏；`HistorySection` 会在每个 `clipboard.captured` 上重复 `load(0,false)`（刷新风暴）。

**解决方案**：
```tsx
useEffect(() => {
  let unlisten: UnlistenFn | undefined;
  let cancelled = false;
  void onClipboardCaptured(() => { void load(0, false); }).then((u) => {
    if (cancelled) u(); else unlisten = u;       // ← 迟到则立即注销
  });
  return () => { cancelled = true; unlisten?.(); };
}, [load]);
```

**前后对比**：修复前可能泄漏并重复处理；修复后与其余 15 处一致。

**预防措施**：抽 `useTauriEvent(topic, handler)` hook（内部统一处理 cancelled/卸载），`src/ipc` 暴露；禁止业务组件直接 `listen()`。项目已有 15 处手写范式，收敛后还能消除重复代码。

---

# COR-18 · Vault 字段以数组下标作 key → 明文显示态残留　`P2` `[实测]`

**位置**：[vault/VaultPanel.tsx:906-908](../../src/modules/vault/VaultPanel.tsx#L906-L908)（`<FieldValue key={i} .../>`）、[:198](../../src/modules/vault/VaultPanel.tsx#L198)（`shown` 组件内 state）

**问题**：`entry.fields.map((f, i) => <FieldValue key={i} field={f} entryId={entry.id} />)` 用**位置索引**作 key（外层条目已用 `entry.id`）。`entries_changed`/解锁刷新后 React 复用同一组件实例 → `shown=true` 被保留。

**影响**：刷新后**新密码可能仍以明文显示**（用户未点击"显示"），或在列表重排时"显示态"错位到别的字段——凭据面板的机密性 UX 缺陷。

**解决方案**：
```tsx
{entry.fields.map((f) => (
  <FieldValue key={`${entry.id}:${f.key}`} field={f} entryId={entry.id} />   // 稳定业务键
))}
```
并在 `FieldValue` 内加"值变更即复位"：
```tsx
useEffect(() => { setShown(false); }, [field.value]);
```

**前后对比**：修复前明文态可能跨数据变更残留；修复后随字段身份重置。

**预防措施**：React `key` 必须用**业务稳定标识**（id/key），禁用数组下标——可加 ESLint 规则（`react/no-array-index-key`）到 `eslint.config.js`；机密字段组件另加"值变即打码"的自保逻辑。

---

# COR-19 · 桌面整理：manifest 写失败无回滚（无法还原）　`P2` `[实测]`

**位置**：[desktop-core/src/tidy.rs:201-225](../../crates/desktop-core/src/tidy.rs#L201-L225)

**问题**：先 `rename` 移动全部文件（`:201` 循环），最后才写 `manifest_path`（`:223`）；manifest 写失败直接 `Err`，无回滚。

**影响**：用户看到"整理失败"，但文件已分散到各分类夹，而 `restore()` 的唯一依据（manifest）不存在 → **无法一键还原**（用户需手工找回）。

**解决方案**：
```rust
// ① 先写"预登记 manifest"（含全部 to→from 计划），再移动
write_manifest_atomic(&self.manifest_path, &Manifest { applied_ms: None, moves: plan.clone() })?;
// ② 逐个移动；任一步失败 → 反向回滚已移动项
let mut done: Vec<MovedEntry> = Vec::new();
for m in &plan {
    match std::fs::rename(&m.from, &m.to) {
        Ok(()) => done.push(m.clone()),
        Err(e) => {
            for d in done.iter().rev() { let _ = std::fs::rename(&d.to, &d.from); }   // 回滚
            return Err(DesktopError::Tidy(format!("移动失败并已回滚: {e}")));
        }
    }
}
// ③ 标记 applied
write_manifest_atomic(&self.manifest_path, &Manifest { applied_ms: Some(now_ms()), moves: done })?;
```
（回滚失败的项必须点名告警，并保留在 manifest 中供 restore 使用。）

**前后对比**：修复前失败即"半整理且不可还原"；修复后要么全部完成，要么回滚到原状。

**预防措施**：批量文件操作统一采用"预登记 → 执行 → 提交/回滚"三段式（`file-core` 的重命名队列、批量删除、系统清理同族）；把"失败可回滚"作为这类功能的验收项。

---

# COR-20 · 系统清理计数谎报　`P2` `[实测]`

**位置**：[sys-core/src/clean.rs:177-184](../../crates/sys-core/src/clean.rs#L177-L184)
```rust
if recycle { recycle_delete(&paths)?; }                       // 返回的删除数被丢弃
else { for p in &paths { let _ = std::fs::remove_file(p); } } // 失败静默
Ok((paths.len() as u64, bytes))                               // 一律报"全部成功"
```
**影响**：被占用/权限失败的文件（`C:\Windows\Temp`、更新缓存常见）仍计入"已删除 N 个 / 释放 X 字节"——用户以为释放成功实际未释放。

**解决方案**：
```rust
let mut removed = 0u64; let mut freed = 0u64; let mut failed = Vec::new();
for p in &paths {
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let ok = if recycle { recycle_delete_one(p).is_ok() } else { std::fs::remove_file(p).is_ok() };
    if ok { removed += 1; freed += size; } else { failed.push(p.clone()); }
}
if !failed.is_empty() {
    tracing::warn!(count = failed.len(), sample = ?&failed[..failed.len().min(5)], "部分文件删除失败（被占用或权限不足）");
}
Ok((removed, freed))       // 只统计真实成功项
```
并让 UI 文案区分"已清理 N 项 / M 项失败（被占用）"。

**前后对比**：修复前谎报；修复后如实计数 + 失败点名。

**预防措施**：把"忽略返回值"作为统计类代码的 review 禁用模式（`let _ =` 出现在可能失败的 IO 上必须解释原因）；`clean`/`uninstall`/`tidy` 类功能统一"逐项结果 + 汇总"数据结构。

---

# COR-21 · 截图入库/OCR 回填吞错（用户看到"完成"但无记录）　`P2` `[实测]`

**位置**：[screenshot-core/src/module.rs:992](../../crates/screenshot-core/src/module.rs#L992)（`finish`）、[:854](../../crates/screenshot-core/src/module.rs#L854)（`scroll_finish`）、[:1419](../../crates/screenshot-core/src/module.rs#L1419)（`record_ocr_text`）

**问题**：`store.insert(&item).ok();`、`store.set_ocr_text(task_id, text).ok();` —— SQLite 写失败（锁/磁盘满）被丢弃；`screenshot.taken` 事件仍发布（[:1000](../../crates/screenshot-core/src/module.rs#L1000)），下游（OCR/上传/通知）以为已入库。

**影响**：用户看到"截图已保存/完成"，历史列表中却没有该条目；OCR 结果丢失但流程显示成功。

**解决方案**：
```rust
match store.insert(&item) {
    Ok(_) => {}
    Err(e) => {
        tracing::warn!(error = %e, path = %item.path.display(), "截图入库失败：历史将缺少该条目");
        // 降级：发布"入库失败"事件 + 保留磁盘文件（不谎报完成）
        bus.publish("screenshot.store_failed", json!({ "path": item.path, "reason": e.to_string() }));
    }
}
```
`record_ocr_text` 需区分"行不存在（正常，条目已被删）"与"写失败（异常）"：前者 debug 日志，后者 warn + 可重试。

**前后对比**：修复前静默丢失 + 谎报完成；修复后失败可见（事件 + 日志 + 磁盘文件保留）。

**预防措施**：确立"**副作用成功才发布成功事件**"的顺序纪律：先落库，再 publish；或在事件载荷中带 `persisted: bool`。把 `.ok()` 用于"数据写入"的位置列入 review 清单（同 `COR-20`）。

---

# COR-22 · 剪贴板导入/批量采纳无事务　`P2` `[实测]`

**位置**：[clipboard-core/src/store.rs:1099-1158](../../crates/clipboard-core/src/store.rs#L1099-L1158)（`import_rows` 逐行提交）、[:975-990](../../crates/clipboard-core/src/store.rs#L975-L990)（`apply_suggestion` 逐条 `execute`）

**问题**：无事务包裹；`import_rows` 文件头只保证"读文件/口令校验"在落库前完成，未覆盖落库中途失败。

**影响**：磁盘满/中途 IO 失败留下**半套导入**或部分应用；`ImportReport` 不返回，用户无法判断。

**解决方案**：
```rust
pub fn import_rows(&self, rows: &[ImportRow]) -> Result<ImportReport, AppError> {
    let conn = self.conn.lock();
    let tx = conn.unchecked_transaction().map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;
    let mut rep = ImportReport::default();
    for r in rows { /* insert via &tx */ rep.imported += 1; }
    tx.commit().map_err(|e| err("CLIPBOARD_STORAGE_001", e))?;    // 全成或全败
    Ok(rep)
}
```
`apply_suggestion` 同样用 `unchecked_transaction()` 包住批量更新。

**前后对比**：修复前可能"半套"且无回执；修复后原子提交 + 返回报告。

**预防措施**：把 `rusqlite::Transaction` 作为**批量写操作的标准写法**；review 清单加"循环内 `execute` 是否有事务包裹"；为导入/批量操作补"中途失败即整体回滚"测试（可用 SQLite 层注入错误，或用只读连接模拟）。

---

# COR-23 · 崩溃恢复记录静默跳过 / 画布损坏静默降级　`P2` `[实测]`

**位置**：[file-core/src/ops.rs:724-742](../../crates/file-core/src/ops.rs#L724-L742)（`pending_ops` 扫描）、[notes-core/src/canvas.rs:24-30](../../crates/notes-core/src/canvas.rs#L24-L30)（`load` 损坏 → 空画布）

**问题**：
```rust
// file-core
if let Ok(raw) = std::fs::read(&p) {
    if let Ok(pending) = serde_json::from_slice::<PendingOp>(&raw) { out.push(pending); }   // 失败静默跳过
}
```
```rust
// notes-core/canvas.rs
serde_json::from_slice::<CanvasDoc>(&bytes).unwrap_or_default()    // 损坏 → 空画布（用户以为画布空）
```

**影响**：未完成操作被静默丢弃（用户无从得知断点丢失）；画布损坏时用户看到空画布，可能**覆盖保存**（此时真数据被清空）→ 不可逆丢失。

**解决方案**：
```rust
// ① file-core：点名 + 计数
let mut skipped = 0u32;
if let Ok(raw) = std::fs::read(&p) {
    match serde_json::from_slice::<PendingOp>(&raw) {
        Ok(po) => out.push(po),
        Err(e) => { skipped += 1; tracing::warn!(path = %p.display(), error = %e, "崩溃恢复记录解析失败，已跳过"); }
    }
}
// 返回值/AcceptOutcome 中带 skipped，UI 提示"其中 M 条恢复记录损坏"

// ② notes-core：区分"不存在"与"损坏"；损坏时进入只读保护
pub fn load(root: &Path, dir_rel: &str) -> Result<CanvasDoc, NoteError> {
    let p = canvas_path(root, dir_rel)?;
    match std::fs::read(&p) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(CanvasDoc::default()),
        Err(e) => Err(NoteError::Io(e)),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            tracing::warn!(path = %p.display(), error = %e, "画布损坏：进入保护模式（禁止覆盖保存）");
            NoteError::Canvas(format!("画布文件损坏：{}（已保护，未覆盖）", e))
        }),
    }
}
```
（前端拿到该错误后禁止保存并提示"另存为/备份后再编辑"。）

**前后对比**：修复前静默丢数据且可被覆盖；修复后损坏可见 + 保护原文件。

**预防措施**："损坏 ≠ 空"原则：所有"解析失败降级为默认值"的写法都要评估"是否会让后续写操作清空真数据"。凡是"读-改-写"同一文件的场景，解析失败必须 fail-closed（见 `COR-05` 同族纪律）。

---

# COR-24 · 解压无 zip-bomb 上限　`P2` `[实测]`

**位置**：[file-core/src/ops.rs:2171-2245](../../crates/file-core/src/ops.rs#L2171-L2245)；同族 [proxy-core/src/sidecar.rs:421-434](../../crates/proxy-core/src/sidecar.rs#L421-L434)（`Vec::with_capacity(f.size())` 用**声明尺寸**预分配）

**问题**：`rep.cur.files_total = archive.len()` 只用于进度；无条目数上限、无单文件/总解压上限，`std::io::copy` 无界写出。`sidecar.rs` 更严重：`f.size()` 来自 zip 头（不受信）直接用于 `Vec::with_capacity` → 恶意包可令宿主尝试巨量分配（OOM/abort）。

**影响**：高压缩比包写满磁盘；sidecar 场景可直接 OOM。

**解决方案**：
```rust
// file-core/src/ops.rs
const MAX_ENTRIES: usize = 100_000;
const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;   // 单文件 2 GiB
const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024 * 1024;  // 总解压 20 GiB

if archive.len() > MAX_ENTRIES { return Flow::msg("压缩包条目数超过上限"); }
let mut total: u64 = 0;
// 每个条目：先看 entry.size()（声明值）做预检，再用 take 限流实际拷贝
if entry.size() > MAX_ENTRY_BYTES { return Flow::msg("单个文件超过解压上限"); }
total += entry.size();
if total > MAX_TOTAL_BYTES { return Flow::msg("累计解压大小超过上限"); }
let mut out = std::fs::File::create(to_long_path(&out_path))?;
let copied = std::io::copy(&mut entry.take(MAX_ENTRY_BYTES + 1), &mut out)?;   // 实际字节兜底
if copied > MAX_ENTRY_BYTES { /* 删除半成品 */ return Flow::msg("解压超出声明大小，已中止"); }

// proxy-core/src/sidecar.rs：改为流式 + 硬上限，不用声明尺寸预分配
const MAX_EXE_BYTES: usize = 256 * 1024 * 1024;
let mut buf = Vec::new();
f.take(MAX_EXE_BYTES as u64 + 1).read_to_end(&mut buf)?;
if buf.len() > MAX_EXE_BYTES { return Err(ProxyError::Sidecar("可执行文件超过上限".into())); }
```

**前后对比**：修复前可写满磁盘 / OOM；修复后条目数与字节双上限，且以实际拷贝字节为准（声明值不可信）。

**预防措施**：把"归档/容器解析"作为一类风险（zip/tar/gzip/7z/pdf 内嵌流），每处都要有"条目数 + 单件 + 总量 + 流式限流"四项；`sidecar` 等"从网络下载后解压"的路径还要加**内容签名校验**（见 `SEC-10`）。

---

# COR-25 · 列表 `limit` 不夹上限 + `host_log` 无长度/换行过滤　`P2` `[实测]`

**位置**：[commands/clipboard.rs:454](../../src-tauri/src/commands/clipboard.rs#L454)（`limit.unwrap_or(100)`）、[proxy.rs:251](../../src-tauri/src/commands/proxy.rs#L251)、[automation.rs:93](../../src-tauri/src/commands/automation.rs#L93)、[notes.rs:272](../../src-tauri/src/commands/notes.rs#L272)、[file.rs:224](../../src-tauri/src/commands/file.rs#L224)；[commands/mod.rs:118-124](../../src-tauri/src/commands/mod.rs#L118-L124)（`host_log`）

**问题**：多处 `limit` 直接透传（同仓 `sync_conflicts_get`/`sync_runs_get` 已夹 200、`sys_processes` 固定 60——语义不一致）；`host_log(level, message)` 无长度上限、无换行过滤，`tracing::warn!(target:"webview", "{message}")` 直写，且 `allow-host-log` 授予**全部 6 个窗口**。

**影响**：前端可传巨值触发大查询/大分配（轻量 DoS）；日志注入（`message` 含 `\n` 伪造日志行）+ 无界日志放大（写满日志文件）。

**解决方案**：
```rust
// 统一常量与助手
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;
fn clamp_limit(l: Option<u32>) -> u32 { l.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) }
// 各命令：let limit = clamp_limit(limit);

// host_log
const MAX_LOG_LEN: usize = 2_000;
let msg: String = message.chars().take(MAX_LOG_LEN).collect::<String>()
    .replace(['\n', '\r'], " ");                 // 剥离控制字符，防日志伪造
tracing::warn!(target: "webview", level = %level, "{msg}");
```

**前后对比**：修复前无上限/可注入；修复后统一夹取 + 清洗。

**预防措施**：把 `limit` 的默认值/上限提升到 `host-core::limits`（项目已有阈值上移的决议 D-16），命令层统一调用 `clamp_limit`；在 `security_config.rs` 加源码扫描断言"命令文件中不出现裸 `limit.unwrap_or`"；`host_log` 收敛为 `tracing` 统一入口（或移除该 IPC，改用既有 `reportError` 通道）。

---

# COR-26 · Mica 无能力探测 → 不支持环境下 chrome 透明（前次 U5 未修）　`P2` `[实测]`

**位置**：[src/layout/MicaBackdrop.tsx:29-35](../../src/layout/MicaBackdrop.tsx#L29-L35)；[tauri.conf.json:22-27](../../src-tauri/tauri.conf.json#L22-L27)（`transparent: true` + `windowEffects: mica`）

**问题**：
```tsx
const IN_TAURI = "__TAURI_INTERNALS__" in window;
export default function MicaBackdrop() { if (IN_TAURI) return null; ... }   // 无任何能力探测
```
配置层无条件 `transparent: true` + `mica`。

**影响**：Win10、远程桌面会话、部分 VM 上 Mica 不可用 → 三条 chrome（标题栏/导航/状态栏）背景直接透出壁纸/桌面，可读性崩坏（前次报告 U5 已指出，**未修**）。

**解决方案**：
1. Rust 侧新增能力查询（复用已注册端口或新增轻量命令）：
```rust
#[tauri::command]
pub fn host_capabilities() -> CapabilitiesDto {
    CapabilitiesDto { mica: win_integration::dwm::mica_supported(), /* build >= 22000 且非远程会话 */ }
}
```
```rust
// win-integration/src/dwm.rs
pub fn mica_supported() -> bool {
    // ① Windows 11 内部版本 ≥ 22000；② 非远程桌面会话；③ 非被禁用的合成
    os_build() >= 22000 && !is_remote_session() && dwm_composition_enabled()
}
```
2. 前端按能力决定是否启用透明；不支持时走渐变回退（`micaFallback` 已是现成回退）：
```tsx
const caps = useCapabilities();                  // 启动时一次
if (!caps.mica) return <div className={styles.micaFallback} />;
```
3. 配置层：`transparent` 保持 true（不透明回退由前端铺底），但**窗口效果**在运行时不支持时不做任何事（Tauri 会忽略），关键是把"视觉回退"控制在前端。

**前后对比**：修复前 Win10/RDP 透壁纸；修复后按能力回退为可读背景。

**预防措施**：所有"OS 版本/硬件相关效果"（Mica、Acrylic、GPU 加速、Windows Hello、ConPTY、虚拟网卡）都要有**能力探测 + 回退路径 + 真机验收项**三元组；在 `security_config.rs` 之外再加一组 `capability_config.rs` 式回归测试（Fake 能力返回不支持时断言回退渲染）。

---

# COR-27 · 隐式覆盖/重置类操作缺确认　`P2` `[实测]`

**位置**：[editor/EditorPanel.tsx:395-402](../../src/modules/editor/EditorPanel.tsx#L395-L402)（另存为覆盖既有文件）、[desktop/DesktopPanel.tsx:597-603](../../src/modules/desktop/DesktopPanel.tsx#L597-L603)（回退内置六类，单击即清空自定义 `tidy_map`）

**问题**：二者均为破坏性但无确认（同面板其他操作如 PDF 压缩/水印、删除随记/整理/还原都已有 `confirmAction`）。

**影响**：误填已存在路径即静默覆盖（无备份）；一次误点即丢失全部自定义分类映射（无快照/无撤销）。

**解决方案**：
```tsx
// EditorPanel 另存为
const info = await editorTargetExists(target);          // 新增轻量命令，或由 saveAs 返回 exists 标志
if (info.exists && !(await confirmAction({ title: "覆盖已存在的文件？", message: target, confirmLabel: "覆盖", danger: true }))) return;
await editorSaveAs(id, target);

// DesktopPanel 回退内置
if (await confirmAction({
  title: "回退为内置六类分类？",
  message: `将清除 ${customCount} 条自定义映射，且无法撤销。`,
  confirmLabel: "回退", danger: true,
})) { await saveTidyMap(null); }
```
（更佳：回退前自动备份 `tidy_map` 到设置历史，支持一键恢复。）

**前后对比**：修复前静默覆盖/丢失；修复后确认 +（可选）可恢复。

**预防措施**：**建立"破坏性操作清单"并制度化**：所有"覆盖/删除/重置/解除"操作必须出现在清单中，每项标注"确认方式 + 是否可撤销 + 测试名"（本次审查已产出该清单，见 `07-prevention.md §4`）；新增操作时清单与代码同 PR 更新。

---

# COR-28 · `insert_encrypted` 无 blob 分支（超阈值密文内联主表）　`P3` `[走查]`

**位置**：[clipboard-core/src/store.rs:335-365](../../crates/clipboard-core/src/store.rs#L335-L365)

**问题**：加密落库路径不判 `BLOB_THRESHOLD`，密文 base64 直接写 `content` 列 → 极长敏感文本的密文（base64 膨胀 4/3）内联进主表，违反"大对象（>64KB）存 blob"硬约束与 `insert_row` 的既有语义。

**解决方案**：与 `insert_row` 统一策略——密文超阈值时写 blob（密文已是安全内容），主表存 `blob_path` + 截断标记；读取侧由 `COR-01` 的统一读取函数处理（密文从 blob 读出后解密）。

**前后对比**：修复前主表膨胀、索引与查询变慢；修复后与明文路径一致，主表保持瘦。

**预防措施**：`BLOB_THRESHOLD` 判定下沉为 `store` 内的单一 `fn store_payload(...)`，明文/密文/图片/文件四条路径共用；加"两路径行为一致"的参数化测试。

---

# COR-29 · `remote_block_on` 用 `expect` 在 runtime 停摆时 panic　`P3` `[走查]`

**位置**：[file-core/src/remote/mod.rs:259-268](../../crates/file-core/src/remote/mod.rs#L259-L268)
```rust
rx.recv().expect("file-core remote runtime 意外停摆，远端请求无法收取")
```
**问题**：专用 runtime 若停摆，`recv` 返回 `Err` 直接 panic，打断调用它的 worker 线程（非隔离崩溃）。方向与"吞错"相反，但同样违反"错误必须可传播"。

**解决方案**：返回 `Result<T, FileError>`（`FileError::BadState("远端运行时停摆")` + hint "重试或重启远端模块"），由调用臂落 `Flow::Failed`。

**前后对比**：修复前 panic（可能带走 worker/整个远端流程）；修复后可控失败 + 可重试。

**预防措施**：生产代码（非测试）禁用 `unwrap/expect/panic!`——本次统计 commands 层为 0 处（好），但 crate 内非测试路径仍有约 9 处；建议加 clippy `unwrap_used`/`expect_used` 到非测试目标（`[workspace.lints]`）并逐条豁免。

---

# COR-30 · `start_dispatcher` 非幂等（未 stop 直接 start 会泄漏线程）　`P3` `[走查]`

**位置**：[automation-core/src/module.rs:352-448](../../crates/automation-core/src/module.rs#L352-L448)
```rust
let sched_token = Arc::new(AtomicBool::new(false));
*self.cancel = sched_token.clone();          // ← 覆盖旧令牌
std::thread::spawn(move || { ... let cancel = sched_token; ... });   // 旧线程持旧 Arc
```
**问题**：新 token 覆盖 `*self.cancel`，旧调度线程捕获的是旧 Arc（永不置真）→ 未 stop 直接 start 会永久泄漏旧线程与订阅（标准 `stop→start` 顺序不触发）。

**解决方案**：入口幂等守卫 + stop 时 join：
```rust
fn start_dispatcher(&self, ...) {
    if self.sched_thread.read().is_some() { return; }       // 已启动即返回
    ...
}
fn stop(&self) {
    if let Some(t) = self.cancel.lock().take() { t.store(true, SeqCst); }
    if let Some(h) = self.sched_thread.write().take() { let _ = h.join(); }
}
```
（与 `COR-06` 的取消令牌治理同批修复：建议统一改用 `CancellationToken`。）

**前后对比**：修复前泄漏叠加；修复后幂等且有 join。

**预防措施**：同 `COR-06`/`COR-07`——"启动/停止必须幂等"写入模块契约；提供 `OnceGuard`/`CancellationToken` 收敛实现。

---

# COR-31 · StatusBar 硬编码假状态　`P3` `[实测]`

**位置**：[src/layout/StatusBar.tsx:131-132](../../src/layout/StatusBar.tsx#L131-L132)
```tsx
<span className={styles.st}>SQLite WAL · 就绪</span>
<span className={styles.hot}>Enter 粘贴</span>
```
**问题**：两条硬编码常量——存储状态与真实健康度无关（永远"就绪"，假绿）；"Enter 粘贴"仅在剪贴板历史列表成立（其他模块为假提示）。

**解决方案**：改为读真实健康态（`host_modules_status` 的存储状态或事件流），未知时显示"检测中"；快捷键提示按当前模块/视图动态派发（已有 `activeModule`/`isClipView` 判定）。

**前后对比**：修复前永远"就绪"+ 假快捷键提示；修复后反映真实状态与上下文。

**预防措施**：**禁止在状态栏/角标类 UI 出现常量文案**（会误导用户对系统状态的判断），review 清单加"该文案是否会随真实状态变化"；项目已有真实状态源（`stores/modules.ts` 事件流），复用即可。