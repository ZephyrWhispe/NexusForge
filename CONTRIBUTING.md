# 贡献指南 / Contributing

> 本文件同时是**编码纪律速查**：新增代码评审时逐条对照（多数条目来自历史缺陷复盘，出处见 docs/ 下各审查报告与 DECISIONS.md）。

## 工作流

1. 从 `master` 拉分支（修复/特性一题一分支）。
2. 提交信息：`batch Bx T-Bx-y <一句话>`（批次任务制，见 docs/panels/ 任务书惯例）。
3. PR 门禁 = CI 全绿：`cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / 前端 `tsc + eslint + vitest`。

## Rust 纪律

- **原子写**：所有落盘一律 `host_core::util::write_atomic`（同目录 tmp + fsync + rename）。禁止 `std::fs::write` 覆盖既有文件；禁止 rename 前先 `remove_file`（Windows rename 会覆盖，先删制造丢失窗口）。
- **相对路径即安全边界**：外部来源的相对路径必须过段级校验（notes `NoteLibrary::norm_rel` / file-core `safe_rel_path`）。禁止裸 `root.join(user_input)`。Windows 语义陷阱：`\Windows\x` 不是 `is_absolute()` 但 `join` 会替换根；`C:` 前缀同理。
- **AEAD**：会话收发两方向密钥必须分离（`host_core::wire::derive_directional_keys`）；`FrameCipher::open` 自带序号校验，不得绕过。
- **提权边界**：helper IPC 只接受枚举/白名单输入；参数模板表 = 代码常量，新增组合须改代码 + 评审 + 测试。
- **信任根 fail-closed**：identity/paired/known_hosts 不可解 → 报错留证，绝不覆盖重生。
- **回调/钩子路径**：禁止 IO、大分配、多次取锁（低级钩子在系统输入路径上同步执行）。
- **"区分缺失与损坏"**：`match read { NotFound => create, Ok(bytes) => parse_or_fail, Err(e) => fail }`；`if let Ok(bytes) = read` 吞错写法禁用。
- **spawn 归属**：每个 `tokio::spawn`/`thread::spawn` 的句柄必须入结构体并在 stop 收回（abort/join）；start 必须复位取消信号（可重启契约）。
- **游标/进度只在成功后推进**；数据类失败必须回报到 UI（AppError + hint），`tracing::warn!` 不是用户可见面。
- **文本回写编码闭环**：读→改→写同一文件必须携带原编码；`from_utf8_lossy` 的结果禁止回写。

## TypeScript/React 纪律

- **事件只作门铃**：`nf:event` 回调只读 `topic`，事实源 = 重新 invoke 命令。
- **异步响应守卫**：用户输入驱动的查询一律带序号守卫（seq ref）+ 防抖；卸载守卫（`cancelled` 标志）用于所有 `listen().then()`。
- **渲染净化**：`dangerouslySetInnerHTML` 仅允许出现在 `src/components/MarkdownView.tsx`（security_config 断言会拦）。
- **表单持久化**：禁止"每击键即写盘"；防抖合并 + 失败显错，不静默。
- **React key**：用业务稳定标识，禁数组下标。
- JSX 字符串里的 Windows 路径示例必须 `\\` 转义（`\r` `\n` 会变成控制符）。

## 安全评审四问（任何涉及 IPC/路径/渲染/提权的改动）

1. 输入来自谁？（本机用户 / 已配对对端 / 远端服务器 / 任意窗口）→ 按最不可信校验。
2. 写盘位置能否越出预期根？段级校验 + `starts_with` 复核。
3. 副作用可否幂等还原？系统级修改（注册表/服务/防火墙）必须"备份（含实际写入值）+ 还原 + 启动扫描 + 退出钩子 + 崩溃恢复测试"五件套。
4. 负例测试在哪？每个安全修复必须带"先失败后通过"的负例。
