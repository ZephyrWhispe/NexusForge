# 05 第二阶段模块细化（proxy / vault / file / desktop / kvm）

> 依赖：阶段一（S1–S7 + C/P/O 出口）｜ 细化深度：步骤级 + 关键结构 + 算法要点 + 风险标注。
> 推荐实施顺序：**K1 键鼠共享 → V 密码库 → F 文件 → PR 代理 → D 桌面效率**（K 最独立先做；V 的加密原语被 PR 复用；PR 风险最高放后，且需回改 host 崩溃恢复注册点）。

---

## K kvm-core 键鼠共享

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| K1 | 设备发现：UDP 组播心跳 | S3 |
| K2 | 配对：一次性码 + 公钥指纹校验 | K1 |
| K3 | 会话层：TCP 帧协议 + 消息类型 | K2 |
| K4 | 输入捕获（服务器端） | K3 |
| K5 | 输入注入（客户端） | K3 |
| K6 | 剪贴板/文件通道 | K3, C3 |
| K7 | 屏幕边缘切换算法 | K4,K5 |

### 关键结构
```rust
// K1 心跳协议：UDP 组播 239.255.42.98:49800，每 1s
// payload(JSON): { device_id, device_name, pubkey_fingerprint, tcp_port, caps:["input","clip","file"], seq }
// 超时判定：5s 未收到 → 标记离线；收到重复 device_id 以 seq 最新为准

// K3 帧协议（TCP， length-prefixed）：
// [u32 len][u8 msg_type][u8 flags][payload]
// msg_type: 0x01 Hello 0x02 InputEvent 0x03 ClipData 0x04 FileChunk 0x05 FileMeta 0x06 Ack 0x07 Ping
// 会话密钥：X25519 协商 + HKDF → ChaCha20-Poly1305 全帧加密（flags 含重放计数器）

// K4 捕获：低级键盘/鼠标钩子（SetWindowsHookEx WH_KEYBOARD_LL/WH_MOUSE_LL）在专用线程，
//   事件节流：鼠标 move 采样 ≤ 125Hz；服务端开启"接管模式"时本地事件抑制（不回注本机）
// K5 注入：SendInput（绝对坐标按目标屏分辨率归一化 0..65535）
```

### 算法：K7 边缘切换
```
服务端维护 [设备→屏幕边缘] 映射。鼠标 x 抵达映射边缘（容差 2px，100ms 冷却）→
① 锁定本地输入钩子 ② InputEvent 交由目标设备客户端注入 ③ 目标端鼠标回移回该边缘时反向交还。
切回条件优先级：边缘回移 > 快捷键 Ctrl+Alt+Shift+Q。
```

### 风险标注
- 钩子回调必须快（<5ms），任何慢逻辑丢进 channel 由 worker 处理，否则系统移除钩子。
- Windows 会话隔离：服务模式（Windows 服务）下注入不能作用于用户桌面——v1 仅做**同会话用户态**实现，服务模式列入 backlog。
- 文件传输：复用 04 FileChunk 协议，4MB/块 + 全文件 SHA256 终验；断点续传按 (file_hash, chunk_index) 位图。

---

## V vault-core 安全与凭据

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| V1 | 加密核心：Argon2id KDF + AES-256-GCM 信封 | S2 |
| V2 | 保险库数据模型（条目/文件夹/TOTP 字段） | V1 |
| V3 | 解锁/锁定流程（内存密钥生命周期） | V1 |
| V4 | Windows Hello 解锁（HelloPort） | V3 |
| V5 | 自动锁定（空闲/窗口失焦计时） | V3 |
| V6 | 密码生成器 | V1 |
| V7 | IPC + UI（解锁页/条目列表/编辑器） | V2–V6 |

### 关键结构
```rust
// V1 信封格式：{ kdf: {algo:Argon2id, m_cost:64MiB, t:3, p:4, salt}, nonce, ciphertext, aad: vault_id }
// 主密钥派生：master_password --Argon2id--> KEK；DEK(随机32B) 被 KEK 包裹存头部；改密码只重包 DEK
// 内存纪律：所有密钥类型 = SecretBox<[u8;32]>（zeroize Drop）；VirtualLock 锁页

// V2 模型：entries(id, title, folder_id, fields JSON加密, totp_secret 加密, updated_at)
//   fields: [{key:"password", kind:"password", value:SecretString}, {kind:"url|note|otp|..."}]
// V3 解锁态：Arc<VaultUnlocked{ dek: SecretBox } > 存全局 Option；锁定 = drop（密钥清零）+ 清缓存
// V4 Hello：启用时用 Hello 派生的 DPAPI 保护密钥包裹 DEK（免密解锁路径）；指纹校验失败计数 ≥5 → 强制密码解锁
// V5 自动锁：默认失焦 5min / 空闲 15min（可配）；锁定前 30s 托盘气泡预警
// V6 生成器：charclass 组合 + SecurityRandom 选字符；策略校验（至少 3 类）；避免易混淆字符集可开关
// V7 TOTP：RFC 6238，HMAC-SHA1，30s 步长；剩余时间环形进度条（前端 rAF）
```

### 风险标注
- **核心转储**：进程默认可 dump —— `SetProcessMitigationPolicy` 关闭或至少文档声明；内存密钥禁止 `Debug/Display`（clippy 加 `#[deny]` 约束：包装类型不派生 Debug）。
- 剪贴板联动：复制密码走 ClipboardPort 时设置**90s 自动清除**定时（复用 C3 回写窗口机制）。
- DB：解锁态下字段缓存禁止整库解密落盘明文临时文件。

---

## F file-core 文件与存储

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| F1 | 文件浏览服务（列目录/排序/面包屑） | S3 |
| F2 | 异步操作队列（复制/移动/删除/压缩） | F1 |
| F3 | 冲突策略（同名：跳过/覆盖/重命名/比较） | F2 |
| F4 | 预览服务（图片/文本/视频缩略图） | F1 |
| F5 | USN 全局搜索（UsnIndexPort） | S3 |
| F6 | StorageDriver 网盘抽象 + 注册表 | F2 |
| F7 | 批量重命名 DSL | F1 |

### 关键结构
```rust
// F2 操作队列：全局 mpsc 队列 + N worker(默认2)；
//   Op::{Copy{src,dst}, Move, Delete, Compress, Extract}；每 Op 产生 progress 事件（节流 200ms）
//   断点续传：大文件分 4MB 块，进度存 {appData}/pending_ops/{op_id}.json（崩溃恢复扫描点，见 01 S6.5）
// F3 策略枚举逐文件询问 UI（operation.conflict 事件 → IPC 应答），批处理可"应用到全部"
// F6 trait StorageDriver: list/read/write/mkdir/remove/move/quota；
//   注册表静态内置（local/smb/ftp/webdav/s3）+ 动态（rclone sidecar 包装驱动）；UI 呈现统一目录树
// F7 规则：{name}{ext} 变量替换 + 序号补零 + 正则替换 + 大小写转换；预览前 20 条再应用
```

### 风险标注
- 路径：一律 `std::path::PathBuf`，禁字符串拼接；>260 字符路径统一走 `\\?\` 前缀。
- 删除回收站：`SHFileOperationW` 带 `FOF_ALLOWUNDO`；secure delete 才直删（覆写 1 遍 + truncate）。
- USN：需管理员权限读 MFT —— 无权限时降级 `FindFirstFileEx` 遍历并在 UI 标注"索引受限"。
- 云驱动网络抖动：所有 driver 调用套 10s 超时 + 指数退避重试 2 次。

---

## PR proxy-core 代理与 VPN

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| PR1 | 内核抽象：`KernelDriver` trait（sing-box 首选） | S3 |
| PR2 | Sidecar 管理（下载/校验/守护/版本通道） | PR1 |
| PR3 | 配置生成（订阅→节点→出站/分流规则 JSON） | PR2 |
| PR4 | 系统代理设置（注册表 + WinINET 刷新 + 恢复） | PR2 |
| PR5 | TUN 模式（互斥系统代理） | PR2 |
| PR6 | IPC/UI（节点延迟测试、日志查看） | PR3–PR5 |

### 关键结构
```rust
#[async_trait]
pub trait KernelDriver: Send + Sync {
    fn id(&self) -> &'static str;                       // "sing-box"
    async fn start(&self, cfg: PathBuf) -> Result<KernelHandle, AppError>;  // handle: 停止/日志流/健康检查
    fn config_schema(&self) -> serde_json::Value;
}
// PR4 系统代理：写 HKCU\...\Internet Settings（ProxyEnable/ProxyServer/Override）
//   → InternetSetOption(INTERNET_OPTION_SETTINGS_CHANGED + REFRESH) 广播生效
//   恢复：启动时记录原值到 {appData}/proxy_backup.json；停止/退出/panic hook 还原（01 S6.5 已注册钩子位）
// PR5 TUN：需要管理员（wintun.dll sidecar）；开启 TUN 时强制关系统代理（互斥开关，UI 提示）
```

### 风险标注
- **合规**：只做框架不内置节点/订阅；README 首屏声明；分发避开商店（DESIGN §10.5）。
- 崩溃残留代理 = 用户断网最高危场景：还原逻辑必须同时挂 panic hook、退出钩子、启动扫描三处。
- 杀软误报：仅使用官方签名内核 + 记录 THIRD_PARTY_LICENSES。
- 订阅拉取失败重试上限 3 次；UA 标识应用版本；订阅内容视为敏感（内存中处理，不落明文日志）。

---

## D desktop-core 桌面效率

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| D1 | 快速启动器（索引构建 + 呼出面板） | S6 |
| D2 | 模糊匹配打分 | D1 |
| D3 | 桌面格子整理 | D1 |
| D4 | 待办与随记（SQLite + 快捷键速记） | D1 |

### 关键结构
```rust
// D1 索引源：开始菜单(lnk 解析) + PATH 可执行 + 设置中心注册的动作(剪贴板/截图/OCR 快捷入口)
//   索引内存 HashMap，启动异步构建（不阻塞），fs watcher 增量更新
// D2 打分：score = 0.5*前缀命中率 + 0.3*子序列连续度(fuzzy match 简化版) + 0.2*使用频次(衰减计数)
//   频次：count*exp(-Δdays/30)；仅前 10 条进 UI（虚拟列表）
// D3 格子：解析桌面图标位置（ListView 控制），按扩展名/类别分格渲染为置顶面板，提供"整理/还原"
//   —— Windows 11 桌面 API 不稳定，v1 仅提供"归类快捷方式生成到分类文件夹"的安全实现
// D4 速记：全局快捷键 → 顶部速记条 → 存 {appData}/db/desktop.db；支持 #标签、明天/周几 解析为提醒
```

### 风险标注
- 启动器禁止在 UI 线程同步枚举 PATH；动作执行 `ShellExecuteExW`，管理员目标走 runas 提权确认。
- 待办提醒用 Task Scheduler 注册一次性任务（经 host Port），避免常驻定时器遗漏。

## 阶段二验收

- [ ] 两台设备 10 分钟内完成配对并稳定共享键鼠/剪贴板（丢包重传 < 1s）
- [ ] 密码库：错误密码 5 次锁定；锁定后内存密钥 zeroize（测试断言缓冲区全 0）
- [ ] 文件复制 1GB 可暂停/恢复，崩溃后可从 pending_ops 继续
- [ ] 代理：开启→断网场景（kill -9）→ 重启软件自动恢复原代理设置
- [ ] 启动器呼出 < 100ms，索引 1 万条 < 50MB 内存
