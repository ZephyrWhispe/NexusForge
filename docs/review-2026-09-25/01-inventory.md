# 01 · 问题分类清单（按严重程度排序）

> 共 **99** 条：P0 ×1 ｜ P1 ×19 ｜ P2 ×50 ｜ P3 ×29
> 类型说明：安全 / 功能缺陷（正确性、数据完整性）/ 性能 / 规范 / 治理 / 文档 / 测试
> 位置均为相对仓库根的真实路径与行号；`[实测]`=作者逐行复核确认，`[走查]`=代码阅读推理，`[框架]`=依赖框架/OS 语义推理。
> 详细解决方案见 `02-`…`06-` 各文档，列内"详见"给出 ID。

---

## P0 · 阻断（1）

| ID | 类型 | 位置 | 问题 | 影响 | 证据 |
|---|---|---|---|---|---|
| **SEC-01** | 安全 | [kvm-core/src/session.rs:274,304](../../crates/kvm-core/src/session.rs#L274)；[sync-core/src/transport.rs:175-176,205-206](../../crates/sync-core/src/transport.rs#L175-L176)；[host-core/src/wire.rs:180-223](../../crates/host-core/src/wire.rs#L180-L223) | 收/发两个 `FrameCipher` 用**同一把会话密钥**且计数器**都从 0 开始**，nonce 空间完全重叠 | ChaCha20-Poly1305 nonce 复用：两方向第 N 帧密钥流相同 → 窃听者 `C1⊕C2` 还原明文；同 key 同 nonce 使 MAC 密钥重复 → 可伪造帧；`open()` 不校验 nonce 序号 → 可重放。影响 KVM 遥控输入/剪贴板/文件与同步全部载荷的机密性与完整性 | `[实测]` |

## P1 · 严重（19）

| ID | 类型 | 位置 | 问题 | 影响 | 证据 |
|---|---|---|---|---|---|
| **SEC-02** | 安全 | [commands/winops.rs:35,47](../../src-tauri/src/commands/winops.rs#L35)；[sys-core/src/winops.rs:241-259](../../crates/sys-core/src/winops.rs#L241-L259)；[win-integration/src/maintenance.rs:26,60-74](../../crates/win-integration/src/maintenance.rs#L60-L74)；[nexusforge-helper/src/dispatch.rs:172-189](../../crates/nexusforge-helper/src/dispatch.rs#L172-L189) | 提权 helper 的 `exec` 把调用方 `args` 逐字透传（模块文档明令"禁止透传任意字符串"）；且外置 catalog 位于**用户可写**的 `{appData}/winops/catalog`，同 id 可覆盖内置 tweak | 用户级进程落一个 JSON（或覆盖内置项）+ 一次 UAC → 以提权身份执行 `netsh`/`dism`/`sfc`/`powercfg` 任意参数、任意 HKLM 写、任意服务启停 | `[实测]`+`[框架]` |
| **SEC-03** | 安全 | [commands/notes.rs:405-439](../../src-tauri/src/commands/notes.rs#L405-L439)；[notes-core/src/canvas.rs:14-21,33-42](../../crates/notes-core/src/canvas.rs#L14-L21) | `notes_canvas_get/save` 的 `dir` 原样进入 `canvas_path`（无 `norm_rel`、无 `..`/绝对路径校验），并 `create_dir_all(parent)` 后写盘 | `dir="..\\..\\Windows"` 或绝对路径 → 在任意目录创建/读取/覆写 `.nforge-canvas.json` | `[实测]` |
| **SEC-04** | 安全 | [notes-core/src/library.rs:52-75](../../crates/notes-core/src/library.rs#L52-L75)；调用点 [commands/notes.rs:86,122,143,160](../../src-tauri/src/commands/notes.rs#L86) | `norm_rel` 只拒空段/`.`/`..`，**不拒盘符前缀**；`disk()` 用 `root.join(rel)`，Windows 下带 prefix 的路径会**替换根** | `notes_create("C:\\Users\\x\\Desktop\\pwn.md")` 及 `write/delete`（仅校验 `.md` 后缀）可读写在笔记库根之外任意 `.md` | `[实测]`+`[框架]` |
| **SEC-05** | 安全 | [file-core/src/ops.rs:2192-2199](../../crates/file-core/src/ops.rs#L2192-L2199) | zip-slip 防护用 `name.contains("..") \|\| Path::new(&name).is_absolute()`；Windows 下 `\Windows\x` 既不含 `..` 也**不是 absolute**（有根无前缀），而 `join` 会替换除前缀外的全部 | 构造条目名 `\Windows\System32\xx.dll` 的压缩包 → 解压写出到同盘任意路径（含 System32） | `[实测]`+`[框架]` |
| **SEC-06** | 安全 | [file-core/src/ops.rs:1589-1596](../../crates/file-core/src/ops.rs#L1589-L1596) | 下载落点 `dst_local.join(full_rel)`，`rel` 来自**远端返回的条目名**（`walk_remote` → `e.name`），只把 `/` 换成 `\`，不过滤 `..`/`\`/绝对路径 | 恶意/被攻陷的 WebDAV/SFTP/FTP 端返回 `..\..\evil` → 在用户选定目录之外任意位置写文件 | `[实测]` |
| **SEC-07** | 安全 | [NotesPanel.tsx:988](../../src/modules/notes/NotesPanel.tsx#L988)；[EditorPanel.tsx:676](../../src/modules/editor/EditorPanel.tsx#L676) | `marked.parse()` 输出直接 `dangerouslySetInnerHTML`，`marked@15` 已无 `sanitize` 选项，全库无 DOMPurify | 笔记 `.md` 经 sync 跨设备同步（`sync-core` 把 `notes` 列入数据集）→ 对端写 `<img src=x onerror=…>` 即在 WebView 执行 JS；可调 `__TAURI_INTERNALS__.invoke` 触达该窗 capability 下的 IPC | `[实测]` |
| **SEC-08** | 安全 | [tauri.conf.json:30-36](../../src-tauri/tauri.conf.json#L30-L36) | `assetProtocol.scope = ["$APPDATA/**"]`（应用数据根），且 CSP `img-src` 放行 `http://asset.localhost` | 任一窗口（含 overlay/pin/quickpanel）可读数据目录下全部文件：config、log、导出物、rules.json、加密库文件；超出图像展示所需 | `[实测]` |
| **SEC-09** | 安全 | [proxy-core/src/sysproxy.rs:104-120](../../crates/proxy-core/src/sysproxy.rs#L104-L120)；[proxy-core/src/service.rs:303,705-712](../../crates/proxy-core/src/service.rs#L303) | 残留识别用**当前** `mixed_port` 比对注册表值；而 `set_mixed_port` 只改内存+持久化、不重写注册表 | 开启系统代理后改端口再被强杀 → 下次启动判定"用户改过"→ 删除备份且不还原 → 系统代理永久指向死端口（断网） | `[实测]` |
| **COR-01** | 功能缺陷 | [clipboard-core/src/store.rs:300-309,798-838](../../crates/clipboard-core/src/store.rs#L300-L309) | `>64KB` 文本写 blob 时把 `content` 列写成**空串**；`get_payload` 文本臂只读 `content`、**不读 blob** | 复制一次 >64KB 纯文本：列表预览空白、`clipboard_paste`/栈粘贴得到**空字符串**（真实内容只在 blob 文件里） | `[实测]` |
| **COR-02** | 安全/功能缺陷 | [automation-core/src/wasm.rs:148-163](../../crates/automation-core/src/wasm.rs#L148-L163) | host 函数读 guest 字符串时**先按 guest 给的 `len` 分配宿主堆**，再做边界校验 | 插件调 `nf.log(0, 0x7FFFFFFF)` 即触发 ~2GB 分配 → OOM/abort；绕过了 wasmtime 的 64MB `StoreLimits`（只约束 guest 内存） | `[实测]` |
| **COR-03** | 功能缺陷 | [vault-core/src/vault.rs:155-156,193-194,378](../../crates/vault-core/src/vault.rs#L155-L156)；[vault-core/src/crypto.rs:70-88](../../crates/vault-core/src/crypto.rs#L70-L88) | `SecretKey` 内联 `[u8;32]`，`dek.lock_in_memory()` 锁的是**移动前**地址，随后 `Inner::Unlocked { dek }` 移动即换地址 | VirtualLock 实际未保护真正的密钥内存（D-24 的"防换页"未达成）；`unlock_memory` 又对从未锁过的地址解锁 | `[实测]` |
| **COR-04** | 功能缺陷 | [editor-core/src/session.rs:301-316,332-342](../../crates/editor-core/src/session.rs#L301-L316) | `save/save_as` 用 `std::fs::write` 直写目标文件（非 tmp+rename），且成功后立即删除 autosave 草稿 | 崩溃/断电/磁盘满发生在写中途 → 用户源文件被截断或半写，且唯一草稿已被删除；全仓其他落盘均为原子写，仅编辑器正文例外 | `[实测]` |
| **COR-05** | 功能缺陷 | [host-core/src/device.rs:79-99,134-146](../../crates/host-core/src/device.rs#L79-L99) | 身份文件"读得到但解不开"（含 `CryptoPort` 缺失导致 DPAPI 解包失败）被当作"损坏"，**直接覆盖**并生成新身份 | device_id 与密钥对永久改变、`paired.json` 全部指纹失配 → KVM/同步信任根被摧毁；且新身份可能以**未保护明文私钥**落盘 | `[实测]` |
| **COR-06** | 功能缺陷 | [sync-core/src/module.rs:1356-1367,1401-1407](../../crates/sync-core/src/module.rs#L1356-L1367)；[sys-core/src/module.rs:145-152](../../crates/sys-core/src/module.rs#L145-L152)；[desktop-core/src/module.rs:157-161](../../crates/desktop-core/src/module.rs#L157-L161) | `stop()` 置 `cancel=true` 后 `start()` **不复位**；sys/desktop 的线程句柄 stop 时不清空 | 走 `host_module_restart`（UI 可达）重启：sync 的 `accept_loop` bind 成功后立即 break（**监听永久失效**）；sys 采样线程与 desktop 提醒线程永不复活（事件静默中断） | `[实测]` |
| **COR-07** | 功能缺陷 | [sync-core/src/module.rs:1368-1396](../../crates/sync-core/src/module.rs#L1368-L1396) | `notes.changed` 订阅任务 `JoinHandle` 不保存、stop 不 abort，循环仅在发送端关闭时 break（总线长期持有 Sender） | 任务永不退出；每次 `start` 再 spawn 一个 → 重启后同一条本地变更被写入两条 op（`Uuid::now_v7()` 无法 `INSERT OR IGNORE` 去重） | `[实测]` |
| **COR-08** | 功能缺陷 | [sync-core/src/module.rs:510-548](../../crates/sync-core/src/module.rs#L510-548) | `apply_remote` 失败仅 `tracing::warn!`，随后仍 `set_cursor(peer, max_ts)` 整批推进 | 磁盘满/权限/占用导致的应用失败 → 该条变更**永久不再重拉**，而面板按协议记账显示"已同步"（谎报进度） | `[实测]` |
| **COR-09** | 功能缺陷 | [notes-core/src/library.rs:77-83,390-402](../../crates/notes-core/src/library.rs#L77-L83) | `read_text` 用 `String::from_utf8_lossy`；`rename` 改写 `[[链接]]` 时以 UTF-8 覆盖回原文件 | GBK 等非 UTF-8 笔记只要含 `[[旧名]]` 且被重命名 → 中文变 `U+FFFD` 后按 UTF-8 写回，**原始字节永久丢失** | `[实测]` |
| **PERF-01** | 性能 | [kvm-core/src/module.rs:839-862](../../crates/kvm-core/src/module.rs#L839-L862) | 提供给低级键盘/鼠标钩子的回调内两次 `parking_lot::Mutex::lock()`（worker 侧持同一锁） | WH_KEYBOARD_LL/WH_MOUSE_LL 回调在系统输入路径上同步执行，锁竞争直接拖慢**全局**键鼠（D-15"回调内不取锁"的反例） | `[实测]` |
| **PERF-02** | 性能 | [settings/SchemaForm.tsx:118-124,178-191](../../src/settings/SchemaForm.tsx#L118-L124) | 文本/多行/SpinButton 每个 `onChange` 发一次 `hostConfigSet`（IPC + 落盘），无防抖、无在途合并；失败不回滚 | 输入一个 10 字符的值 = 10 次 IPC + 10 次写盘；失败时界面仍显示已保存（乐观更新无回滚） | `[实测]` |

## P2 · 中等（50）

### 安全（10）

| ID | 位置 | 问题 | 影响 |
|---|---|---|---|
| **SEC-10** | [proxy-core/src/service.rs:455-490](../../crates/proxy-core/src/service.rs#L455-L490) | 订阅/内核下载：`resp.bytes()` 无大小上限、无 redirect 策略、注释明示"不检查状态码" | 大响应/gzip bomb 内存耗尽；重定向到内网（SSRF 探测）或降级明文 |
| **SEC-11** | [kvm-core/src/pairing.rs:328-441](../../crates/kvm-core/src/pairing.rs#L328-L441) | 6 位一次性配对码与整个握手走**明文 TCP**（会话密钥在配对之后才派生），无 PAKE | 局域网嗅探者在 120s 窗口内抢先以自身公钥发带正确码的 `PairRequest` 完成配对 → 后续可注入输入 |
| **SEC-12** | [proxy-core/src/service.rs:762-792](../../crates/proxy-core/src/service.rs#L762-L792) | TUN 模式全仓无任何防火墙/自身排除规则（System 模式有 bypass 例外） | 依赖内核隐式规避；若内核未排除宿主则存在自旋/环路风险 |
| **SEC-13** | [win-integration/src/clipboard.rs:79,632-663](../../crates/win-integration/src/clipboard.rs#L79) | 防循环用的自写标记是**全局注册的剪贴板格式**，任何进程均可置位 | 本机恶意程序可借此让捕获逻辑直接 `return` → 绕过剪贴板捕获（隐藏窃取痕迹） |
| **SEC-14** | [tauri.conf.json:31](../../src-tauri/tauri.conf.json#L31) | CSP 缺 `object-src`/`base-uri`/`form-action`/`frame-src`；其中 `base-uri` **不回落** `default-src` | `<base href="http://evil/">` 注入可改写全部相对 URL 基准；现有测试只校验 7 条指令存在，给出"CSP 完整"假象 |
| **SEC-15** | [sys-core/src/pkg.rs:222,298-301,325-333](../../crates/sys-core/src/pkg.rs#L222) | scoop 腿经 `cmd /C scoop install <pkg>`：注释与测试断言"零 shell 拼接"，但 `cmd.exe` 自行解析命令行，无空格的 `git&calc` 不会被加引号 | 包名（含 scoop bucket 远端列出的名字）中的 `& \| ^ > < % !` 被当作命令分隔符执行 |
| **SEC-16** | [automation-core/src/wasm.rs:89-105](../../crates/automation-core/src/wasm.rs#L89-L105)；[win-integration/src/shell.rs:158-174](../../crates/win-integration/src/shell.rs#L158-L174) | `nf.open_url` → `ShellExecuteW("open", ...)` 无任何 scheme/路径校验 | 取得 `open` 权限的插件可启动任意本地可执行文件/`.lnk`/自定义协议（`ms-settings:` 等），超出"打开 URL"语义 |
| **SEC-17** | [screenshot-core/src/util.rs:190-213](../../crates/screenshot-core/src/util.rs#L190-L213)；[screenshot-core/src/module.rs:1275-1286](../../crates/screenshot-core/src/module.rs#L1275-L1286) | `filename_template` 用户可填，`resolve_filename` 不剥离路径分隔符，`action_save` 直接 `dir.join(...)` | 模板含 `..\..\` 时写出 `save_dir` 之外 |
| **SEC-18** | [ocr-core/src/tesseract.rs:296-312](../../crates/ocr-core/src/tesseract.rs#L296-L312) | 每次识别把整帧 PNG **明文**写入 `{app_data}/ocr-tmp/{uuid}.png`，成败都 unlink（不覆写）；OCR 模块 init 无 `ocr-tmp` 启动清理（截图模块有 sweep） | 崩溃/强杀后用户屏幕明文截图永久残留（可被文件系统恢复） |
| **SEC-19** | [nexusforge-helper/src/main.rs:51-63](../../crates/nexusforge-helper/src/main.rs#L51-L63) | 看门狗仅在有 `BUSY` 标记时跳过退出，而 `file.clean_dir` 等耗时方法未包 `with_busy` | 清理/删除过程中 helper 可能 `exit(0)` 中断 → 半清理状态 |

### 功能缺陷 / 数据完整性（18）

| ID | 位置 | 问题 | 影响 |
|---|---|---|---|
| **COR-10** | [kvm-core/src/transfer.rs:346-372,377-405](../../crates/kvm-core/src/transfer.rs#L346-L372) | `on_meta` 对 `size/total_chunks/chunk_size` 无上限；`on_chunk` 未校验 `index < total_chunks`，`offset = index * chunk_size`（debug 溢出 panic / release 回绕） | 已配对对端可声明 TB 级 `size` 耗尽磁盘、整数溢出 panic（DoS）或 seek 到错误偏移覆写 |
| **COR-11** | [vault-core/src/vault.rs:449-451](../../crates/vault-core/src/vault.rs#L449-L451) | `persist_header` 先 `remove_file` 再 `rename`（注释"Windows rename 不覆盖"与事实不符，`device.rs` 直接 rename 且正常） | 制造"删旧→写新"非原子窗口：此间崩溃将丢失 `vault.meta.json` → **保险库永久不可解锁** |
| **COR-12** | [vault-core/src/vault.rs:354-367](../../crates/vault-core/src/vault.rs#L354-L367)；[vault-core/src/crypto.rs:303-315](../../crates/vault-core/src/crypto.rs#L303-L315) | DPAPI 解出的明文 DEK `Vec<u8>` 拷贝进 `SecretKey` 后未 `zeroize` 即释放 | 明文密钥残留在已释放堆内存，破坏 vault 的 zeroize 纪律 |
| **COR-13** | [automation-core/src/module.rs:454-464,303-307](../../crates/automation-core/src/module.rs#L454-L464) | `current_hhmm_date` 用 UTC epoch 计算 HH:MM（注释称本地时区）；而 `sync_task` 把同一 `HH:MM` 交给 Windows 计划任务（按**本地**时间） | 中国时区下应用内触发与系统计划任务相差 8 小时，二者重复或错时触发 |
| **COR-14** | [host-core/src/hotkey.rs:114-157](../../crates/host-core/src/hotkey.rs#L114-L157) | `unregister` 只清内存映射，不调 `win.unregister(os_id)`、不清 `os_map`（`HotkeyWinPort::unregister` 已实现但全仓无调用点） | 改键后**旧快捷键在系统层继续生效**并触发原动作；`os_map` 与闭包随注册单调泄漏 |
| **COR-15** | [NotesPanel.tsx:338-353,884,495-508](../../src/modules/notes/NotesPanel.tsx#L338-L353) | `dirty` 存在但 `openNote`/`createNote` 不做未保存校验（EditorPanel 有关闭脏缓冲确认） | 改完未保存点另一篇 → 编辑内容**静默丢失** |
| **COR-16** | [file/FilePanel.tsx:233-249](../../src/modules/file/FilePanel.tsx#L233-L249)；[clipboard/panels/HistorySection.tsx:368-394](../../src/modules/clipboard/panels/HistorySection.tsx#L368-L394) | 目录切换与剪贴板搜索均无请求序号守卫（同仓 `SearchSection`/`LauncherWindow` 已有 `seq` 范式） | 快速连点目录/逐字输入时，慢响应覆盖新响应 → 界面显示与 `cwd`/输入框不一致 |
| **COR-17** | [HistorySection.tsx:430-432](../../src/modules/clipboard/panels/HistorySection.tsx#L430-L432)；[clipboard/panels/SettingsSection.tsx:178-180](../../src/modules/clipboard/panels/SettingsSection.tsx#L178-L180)；[vault/VaultPanel.tsx:404-406](../../src/modules/vault/VaultPanel.tsx#L404-L406) | `listen()` 的 `.then(u => unlisten = u)` 无 `cancelled` 守卫（其余 15 处监听都有） | 卸载早于 listen resolve 时迟到监听器永不注销 → 监听泄漏 + 事件重复处理 |
| **COR-18** | [vault/VaultPanel.tsx:906-908,198](../../src/modules/vault/VaultPanel.tsx#L906-L908) | `entry.fields.map((f,i) => <FieldValue key={i} .../>)`：以位置索引作 key（外层已用 `entry.id`） | `entries_changed`/解锁刷新后组件实例被复用，`shown=true` 跨数据变更保留 → 新密码可能仍处明文态而无用户点击 |
| **COR-19** | [desktop-core/src/tidy.rs:201-225](../../crates/desktop-core/src/tidy.rs#L201-L225) | 先把文件 `rename` 到分类夹，最后才写 manifest；manifest 写失败直接 `Err`，无回滚 | 用户看到失败提示，但文件已移动且 `restore()` 的唯一依据不存在 → 无法一键还原 |
| **COR-20** | [sys-core/src/clean.rs:177-184](../../crates/sys-core/src/clean.rs#L177-L184) | 非回收站臂 `let _ = std::fs::remove_file(p)` 忽略失败，函数一律返回 `paths.len()` 与预算字节 | 被占用/权限失败的文件仍计入"已删除 N 个/释放 X 字节"（谎报） |
| **COR-21** | [screenshot-core/src/module.rs:992,854,1419](../../crates/screenshot-core/src/module.rs#L992) | `store.insert(&item).ok()`、`set_ocr_text(...).ok()` | SQLite 写失败（锁/磁盘）时用户看到"完成"，历史中无此行，`screenshot.taken` 仍发布 → 下游以为已入库 |
| **COR-22** | [clipboard-core/src/store.rs:1099-1158,975-990](../../crates/clipboard-core/src/store.rs#L1099-L1158) | `import_rows` 逐行独立提交无事务；`apply_suggestion` 逐条 `execute` 无事务 | 中途失败留下半套导入/部分应用，且无回执 |
| **COR-23** | [file-core/src/ops.rs:724-742](../../crates/file-core/src/ops.rs#L724-L742)；[notes-core/src/canvas.rs:24-30](../../crates/notes-core/src/canvas.rs#L24-L30) | 崩溃恢复扫描对读/解析失败的 `pending_ops/*.json` 静默跳过（无 warn/无计数）；画布损坏静默降级为空 | 未完成操作与画布内容静默丢失，用户无从得知 |
| **COR-24** | [file-core/src/ops.rs:2171-2245](../../crates/file-core/src/ops.rs#L2171-L2245) | 解压无条目数上限、无单文件/总解压字节上限，`std::io::copy` 无界写出（`entry.size()` 仅用于进度） | zip bomb 写满磁盘 |
| **COR-25** | [commands/clipboard.rs:454](../../src-tauri/src/commands/clipboard.rs#L454)、[proxy.rs:251](../../src-tauri/src/commands/proxy.rs#L251)、[automation.rs:93](../../src-tauri/src/commands/automation.rs#L93)、[notes.rs:272](../../src-tauri/src/commands/notes.rs#L272)、[file.rs:224](../../src-tauri/src/commands/file.rs#L224)；[commands/mod.rs:118-124](../../src-tauri/src/commands/mod.rs#L118-L124) | 多个列表命令的 `limit` 未夹上限（同仓 sync 已夹 200）；`host_log` 无长度上限、无换行过滤 | 前端可传巨值触发大查询（轻量 DoS）；日志注入（`\n` 伪造日志行）+ 无界放大（`allow-host-log` 授予全部 6 个窗口） |
| **COR-26** | [layout/MicaBackdrop.tsx:29-35](../../src/layout/MicaBackdrop.tsx#L29-L35)；[tauri.conf.json:23-27](../../src-tauri/tauri.conf.json#L23-L27) | `transparent: true` + `mica`，但组件无任何能力探测（`if (IN_TAURI) return null`） | Win10/远程桌面/部分 VM 上 Mica 不可用 → 三条 chrome 直接透出壁纸（REVIEW-2026-09-18 的 U5，**未修**） |
| **COR-27** | [editor/EditorPanel.tsx:395-402](../../src/modules/editor/EditorPanel.tsx#L395-L402)；[desktop/DesktopPanel.tsx:597-603](../../src/modules/desktop/DesktopPanel.tsx#L597-L603) | "另存为"覆盖已存在文件无确认；"回退内置六类"单击即清空用户自定义 `tidy_map` 无确认 | 与同面板其他破坏性操作的确认纪律不一致 → 静默覆盖/丢失自定义映射 |

### 性能（7）

| ID | 位置 | 问题 | 影响 |
|---|---|---|---|
| **PERF-03** | [monaco/setup.ts:6](../../src/monaco/setup.ts#L6)；`vite.config.ts:16-21` | `import * as monaco from "monaco-editor"`（全量入口）且无 `build.rollupOptions.manualChunks` | EditorPanel chunk ≈3.24MB、ts.worker ≈5.87MB（REVIEW 的 M10，**未修**） |
| **PERF-04** | [file/FilePanel.tsx:953-1001](../../src/modules/file/FilePanel.tsx#L953-L1001)；[file/RemoteBrowser.tsx:299-331](../../src/modules/file/RemoteBrowser.tsx#L299-L331) | 目录列表 `entries.map` 全量渲染（剪贴板历史已用 `@tanstack/react-virtual`） | 数千~数万条目时滚动/选择卡顿 |
| **PERF-05** | [ocr-core/src/module.rs:188,522-527](../../crates/ocr-core/src/module.rs#L188) | 先 `decode_rgba` 整图入内存，之后才 `downscale_if_needed`（4096 上限） | 20000×20000 PNG 先分配 ≈1.6GB RGBA 再缩图 → 可直接 OOM；`image_b64` 无字节上限 |
| **PERF-06** | [vault/VaultPanel.tsx:161-190](../../src/modules/vault/VaultPanel.tsx#L161-L190) | 每个 TOTP 徽章独立 `setInterval(..., 1000)` 每秒 invoke `vaultTotpNow` | N 个含 TOTP 条目 = N 次/秒 IPC + 每秒重渲染，而 TOTP 仅 30s 变一次 |
| **PERF-07** | [automation-core/src/engine.rs:120-124](../../crates/automation-core/src/engine.rs#L120-L124) | 动作重试用 `std::thread::sleep`（`fire` 是同步在 tokio 任务里调用） | 单个失败动作最多阻塞 tokio worker ≈1.5s，多规则并发挤占运行时线程 |
| **PERF-08** | [host-core/src/registry.rs:164,181](../../crates/host-core/src/registry.rs#L164)；[sync-core/src/module.rs:1382,1462](../../crates/sync-core/src/module.rs#L1382) | `apply_configs`/`apply_one` 与 sync 订阅任务在 async 上下文直接做阻塞 SQLite/文件 IO（init/start/stop 都走 `spawn_blocking`，唯独它们例外） | 阻塞运行时工作线程（含事件转发） |
| **PERF-09** | [clipboard/dib.ts:33-63](../../src/modules/clipboard/dib.ts#L33-L63)；[clipboard/DibThumb.tsx:23-37](../../src/modules/clipboard/DibThumb.tsx#L23-L37) | DIB 逐像素同步解码在主线程（O(w·h)）；缩略图按行独立请求且组件重挂即重取，无缓存 | 8192² 大图约 6700 万次循环 → 主线程卡顿；列表滚动反复请求 |

### 规范（4）

| ID | 位置 | 问题 | 影响 |
|---|---|---|---|
| **STD-01** | [components/DryRunDialog.tsx:108](../../src/components/DryRunDialog.tsx#L108)；[file/NameFixDialog.tsx:111](../../src/modules/file/NameFixDialog.tsx#L111)；[settings/SchemaForm.tsx:209-213](../../src/settings/SchemaForm.tsx#L209-L213) | 2 处 `void props.onConfirm().catch(() => {})` 静默吞错（eslint `no-empty` 不覆盖函数体）；错误解析存在第二套 3 份私有实现（丢弃 `code`/`retryable`） | 未来新调用方未自处理即真丢错；错误文案/分级不统一 |
| **STD-02** | [kvm/KvmPanel.tsx:736](../../src/modules/kvm/KvmPanel.tsx#L736)；[automation/RulesPanel.tsx:867,1094](../../src/modules/automation/RulesPanel.tsx#L867) | JSX 字符串里反斜杠未转义（`"例如 C:\Users\me\Downloads\report.pdf"` → `\r` 变回车控制符） | placeholder 显示错乱（同仓其余处已用 `\\`） |
| **STD-03** | [vault/VaultPanel.tsx:871,899-904,982-989](../../src/modules/vault/VaultPanel.tsx#L871)；[term/TerminalPanel.tsx:856-877](../../src/modules/term/TerminalPanel.tsx#L856-L877)；[OverlayShot.tsx:1588,1591](../../src/windows/OverlayShot.tsx#L1588) | icon-only 按钮缺 `aria-label`（vault 3 处、OverlayShot 撤销/重做）；会话胶囊把 `<span role="button">` 嵌在 `<button>` 内（HTML 非法） | 读屏无法辨识按钮；焦点/Tab 语义歧义 |
| **STD-04** | 见 [05-standards.md §1](./05-standards.md) | 超长文件/单组件：`TerminalPanel.tsx` 1307 行（组件体 ≈1185）、`NotesPanel.tsx` 1284、`RulesPanel.tsx` 1103、`VaultPanel.tsx` 1072、`SysPanel.tsx` 1046 | 单文件多子领域混居，改动风险与评审成本高 |

### 治理（7）

| ID | 证据 | 影响 |
|---|---|---|
| **GOV-01** | 无 `rustfmt.toml`（CI 却在跑 `cargo fmt --all --check`） | 格式随工具版本漂移 |
| **GOV-02** | 无 `deny.toml`，CI 无 `cargo deny`/`npm audit`；而 [THIRD_PARTY_LICENSES.md:6](../../THIRD_PARTY_LICENSES.md#L6) 声明"由批次 2 的 cargo-deny 门禁复核" | 许可合规与已知漏洞审计无自动化（声明与落地脱节） |
| **GOV-03** | 无 `.github/dependabot.yml`（[DESIGN.md:282](../DESIGN.md#L282) 声明"dependabot 自动 PR"） | 依赖更新无自动 PR |
| **GOV-04** | 无 `SECURITY.md`/`CODEOWNERS`/`CONTRIBUTING.md` | 无私密漏洞上报通道与评审归属 |
| **GOV-05** | 无 `.gitattributes`/`.editorconfig`（Windows 开发 + ubuntu CI 跑 fmt/lint） | CRLF/LF 归一缺失 → 跨平台噪声 diff 与 fmt 抖动 |
| **GOV-06** | 根 `Cargo.toml:5-8` 无 `rust-version`/`license`/`[workspace.lints]`；无 `clippy.toml` | MSRV 不可机器判定（DESIGN 称 Rust 1.80+）；lint 不可跨 crate 统一；发布元数据不含 GPL-3.0 |
| **GOV-07** | 工作区遗留 `nf_t23_gates.log`(168KB)/`nf_t7_vitest.log`/`dist/`；`.zcodeignore` 处于**未跟踪**且与 `.gitignore` 重复维护 | 易误入库；双源漂移 |

### 文档（4，其余 10 条 P3 见 [06-docs-inconsistency.md](./06-docs-inconsistency.md)）

| ID | 位置 | 问题 |
|---|---|---|
| **DOC-01** | [DESIGN.md:199,202,247,254-255](../DESIGN.md#L199) | §6"IPC 契约冻结点"的签名/命令名与真实代码不符（`init(&self, &ModuleContext)` vs `Arc<ModuleContext>`；`config_schema -> ModuleConfig` vs `serde_json::Value`；`clipboard_stack_pop` 不存在，实为 `clipboard_stack_paste_next`） |
| **DOC-02** | [impl/03-screenshot-core.md:3,49,59-64,139-142](../impl/03-screenshot-core.md#L3) | 声称 `Windows.Graphics.Capture` 为 v1 主路径、每显示器一覆盖窗、列出 `screenshot_record_start` 等命令——与 DESIGN §11（GDI/PrintWindow）、D-23（v1.1）、`commands/screenshot.rs` 实际命令全部冲突 |
| **DOC-03** | [README.md:3](../README.md#L3)、[DESIGN.md:11](../DESIGN.md#L11) vs [modules.ts:6-46](../../src/layout/modules.ts#L6-L46) | 文档称"13 个模块"、automation 标 P2 无 sync；代码 `ModuleId`/`MODULES` 为 **14** 项且 automation/sync 的 `phase` 均为 P2，测试 `modules.test.ts` 断言 14 |
| **DOC-04** | [panels/2026-09-19/09-screenshot.md:3](../panels/2026-09-19/09-screenshot.md#L3)、[10-ocr.md:3](../panels/2026-09-19/10-ocr.md#L3)、[15-host-shell.md:3](../panels/2026-09-19/15-host-shell.md#L3) | 三份细案"状态：未开工"与"现状问题"（无 screenshot/ocr 目录、MainWorkbench 兜底三元、`moduleId="clipboard"`）均已被交付推翻 |

## P3 · 轻微（29）

### 规范（6）

| ID | 位置 | 问题 |
|---|---|---|
| **STD-05** | 前端量化：magic px **552 处/63 文件**、内联 `style={{` **168 处/28 文件**、硬编码 `#hex/rgba` **≈41 处**（`theme.ts` 57 处为合法主题定义）；[term/ForwardSection.tsx:220](../../src/modules/term/ForwardSection.tsx#L220) `color:"#c50f1f"`、[TerminalPanel.tsx:114](../../src/modules/term/TerminalPanel.tsx#L114) `backgroundColor:"#1b1b1b"` | 不随主题/高对比切换 |
| **STD-06** | [term/TerminalPanel.tsx:96-109](../../src/modules/term/TerminalPanel.tsx#L96-L109)、[editor/EditorPanel.tsx:71-105](../../src/modules/editor/EditorPanel.tsx#L71-L105) vs [components/Tabs.tsx:11-24](../../src/components/Tabs.tsx#L11-L24) | Tab 胶囊样式仍有两处本地副本（共享组件已存在） |
| **STD-07** | [components/Toaster.tsx:89](../../src/components/Toaster.tsx#L89) | 运行时注入 `<style>`（依赖 `style-src 'unsafe-inline'`），与项目"清空 index.html 内联样式"的既定路线冲突 |
| **STD-08** | [layout/panels.tsx:37-59](../../src/layout/panels.tsx#L37-L59) | `ModulePlaceholder`/`placeholderFor` 定义后全库无使用者（死代码） |
| **STD-09** | [win-integration/src/dpapi.rs:22-45](../../crates/win-integration/src/dpapi.rs#L22-L45)（`protect`/`unprotect` 无 `// SAFETY:`、`pbData` 空指针未判）；[usn.rs:136](../../crates/win-integration/src/usn.rs#L136) | `unsafe` 缺 SAFETY 说明与边界校验（前次 M5 未闭环） |
| **STD-10** | [host-core/src/events.rs:63](../../crates/host-core/src/events.rs#L63) vs `file-core/src/conflict.rs:4` | 注册表声明 `operation.conflict` 但两端均无使用者（契约面与事实面漂移） |

### 治理（2）

| ID | 证据 | 影响 |
|---|---|---|
| **GOV-08** | [ci.yml:30-31](../../.github/workflows/ci.yml#L30-L31) 仅 `cargo bench --no-run`；[DESIGN.md:354](../DESIGN.md#L354) 称"CI 每次提交跑基准…退化超阈值即失败" | 无性能回归门禁（阈值断言只由 `perf_thresholds.rs` 随 `cargo test` 承担；启动/内存无自动化） |
| **GOV-09** | [src-tauri/tests/ipc_contract.rs:14-16](../../src-tauri/tests/ipc_contract.rs#L14-L16) | 仅断言 4 个命令的纯函数语义，255 条命令的参数名/返回形状/签名零契约测试 |

### 测试（3）

| ID | 问题 |
|---|---|
| **TEST-01** | `vault` 仅 2 个测试文件（缺打码/揭示/删条目/TOTP 用例）；`RemoteBrowser` 无专用测试（`SEC-06`/`STD-03` 正相关） |
| **TEST-02** | [security_config.rs:149-201](../../src-tauri/tests/security_config.rs#L149-L201) 只校验 7 条 CSP 指令存在，未覆盖 `base-uri`/`object-src`/`form-action`；assetProtocol scope 无任何断言 |
| **TEST-03** | 无路径穿越负例：解压（`SEC-05`）仅跑良性包；重命名（`SEC-04`/`rename.rs`）、画布（`SEC-03`）、远端下载（`SEC-06`）均无用例 |

### 功能缺陷（4）

| ID | 位置 | 问题 |
|---|---|---|
| **COR-28** | [clipboard-core/src/store.rs:335-365](../../crates/clipboard-core/src/store.rs#L335-L365) | `insert_encrypted` 无 `BLOB_THRESHOLD` 分支，超阈值密文 base64 内联主表（违反"大对象走 blob"硬约束） |
| **COR-29** | [file-core/src/remote/mod.rs:259-268](../../crates/file-core/src/remote/mod.rs#L259-L268) | `remote_block_on` 的 `rx.recv().expect(...)`：runtime 停摆即 panic 打断 worker 线程（非隔离崩溃） |
| **COR-30** | [automation-core/src/module.rs:352-448](../../crates/automation-core/src/module.rs#L352-L448) | `start_dispatcher` 非幂等：新 token 覆盖 `*self.cancel`，旧调度线程捕获旧 Arc → 未 stop 直接 start 会永久泄漏旧线程与订阅（标准 stop→start 不触发） |
| **COR-31** | [layout/StatusBar.tsx:131-132](../../src/layout/StatusBar.tsx#L131-L132) | `SQLite WAL · 就绪`、`Enter 粘贴` 为硬编码常量（DB 真实状态如何都显示"就绪"；快捷键提示在其他模块为假提示） |

### 规范（安全类，3）

| ID | 位置 | 问题 |
|---|---|---|
| **SEC-20** | [nexusforge-helper/src/dispatch.rs:75](../../crates/nexusforge-helper/src/dispatch.rs#L75)、[main.rs:85,116](../../crates/nexusforge-helper/src/main.rs#L85) | helper 自建 `Result<_, String>` 错误面，偏离"统一 AppError 错误码体系"（未经裁决） |
| **SEC-21** | [win-integration/src/clipboard.rs:632-663](../../crates/win-integration/src/clipboard.rs#L632-L663) | 与 `SEC-13` 同源：标记机制依赖全局格式存在性，应改为进程内 nonce/时间窗校验 |
| **SEC-22** | [file-core/src/remote/ftp.rs:443-453](../../crates/file-core/src/remote/ftp.rs#L443-L453) | 非回环 FTP 的 `PromptEachTime` 档仍把明文口令送未加密控制连接（已档位确认，登记为设计取舍） |

### 性能（1）

| ID | 位置 | 问题 |
|---|---|---|
| **PERF-10** | [host-core/src/events.rs:321,377,39](../../crates/host-core/src/events.rs#L321) | `subscribe_throttled`/`subscribe_debounced`/`BackpressurePolicy::Throttled` 无任何生产调用方与主题（D-03 声明的"消费端背压统一入口"是死面；仅生产端 `Merged`/`Batched` 在生效） |

### 文档（10）

| ID | 位置 | 问题 |
|---|---|---|
| **DOC-05** | [README.md:13](../README.md#L13) | 写"React 19"，实际 `package.json` 为 `react ^18.3.1`（DESIGN 亦为 18） |
| **DOC-06** | [IMPLEMENTATION.md:20](../IMPLEMENTATION.md#L20) | "决策记录（D-01…D-23）"已过期（实际到 D-33） |
| **DOC-07** | [DECISIONS.md:13-40](../DECISIONS.md#L13-L40) | §1 摘要表止于 D-28，未登记正文的 D-29/D-33；D-30 无定义、D-31/D-32 被引为暂缓但无章节 |
| **DOC-08** | [UI-DEMO.md:21,29,31,69](../UI-DEMO.md#L21)（及代码注释 [MainWorkbench.tsx:35](../../src/windows/MainWorkbench.tsx#L35)、[SubNav.tsx:4](../../src/layout/SubNav.tsx#L4)） | 引用不存在的 DESIGN 小节（§3.2/§3.5/§10"通知系统"） |
| **DOC-09** | [UI-PLAN.md:12,36-64,108](../UI-PLAN.md#L12) | 写"Zustand 4"（实际 5）、§3 目录树与实际结构不符、U4-5 把录屏 UI 列为交付（抵触 D-08 红线） |
| **DOC-10** | [impl/04-ocr-core.md:117](../impl/04-ocr-core.md#L117) | 列出不存在的 `ocr_translate` 命令（D-09 已列 v1.1，但该节未标注） |
| **DOC-11** | [DESIGN.md:99,152,100,157,186](../DESIGN.md#L99) vs §11:383-388 | 同一文件内两套口径并存（Graphics.Capture / FFmpeg 录屏 / OCR 双引擎+翻译 / `blobs/{module}/{yyyy-mm}/{hash}` 均已被 §11 改判） |
| **DOC-12** | [panels/2026-09-19/02-proxy.md:39](../panels/2026-09-19/02-proxy.md#L39) | "系统代理兜底恢复 ✅ D-06"，但 D-06 是"blob 扁平路径"，决策号误引 |
| **DOC-13** | [DESIGN.md:354](../DESIGN.md#L354) vs [ci.yml:30-31](../../.github/workflows/ci.yml#L30-L31) | §9.2 承诺"CI 每次提交跑基准…退化超阈值即失败"，实际仅编译校验 |
| **DOC-14** | [DESIGN.md:84-88](../DESIGN.md#L84-L88) | §2.3 目录结构含不存在的 `resources/`、`src/dock|quick-panels|widgets|overlays`，`crates/` 未列 `nexusforge-helper`/`sync-core` |

---

## 附：清单口径说明

- 本清单**不含**以下已确认无问题的方向（避免虚报）：模块间生产依赖（差集为空）、Windows API 收敛、`permissions/*.toml` 与 255 命令名一致性、SQL 参数化与 LIMIT（clipboard/screenshot/sync/notes/desktop 生产查询均参数化）、FTS 转义、vault 的 Argon2id+AES-GCM+信封加密落地、WASM fuel+64MB 内存限额生效、SSH TOFU 首见即拒+变更即拒、FTP 的 PASV 外泄 IP 拒/CRLF 拒、前端 `any`/`@ts-ignore`/`console` 零命中。
- `SEC-09`、`COR-26`、`PERF-03`、`STD-01` 等取自前次报告但经复核**仍未修复**，故重新登记并保留原证据。