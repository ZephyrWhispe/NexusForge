# 06 第三阶段模块细化（editor / notes / term / sys）

> 依赖：阶段二出口 ｜ 细化深度：步骤级 + 关键结构 + 风险标注

## E editor-core 文本与 PDF

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| E1 | 文件会话模型（打开/脏标记/编码检测） | S3 |
| E2 | Monaco 集成（大文件降级策略） | E1 |
| E3 | Markdown 即时预览（分栏同步滚动） | E2 |
| E4 | PDF 处理（合并/拆分/压缩/水印，qpdf/mutool sidecar） | E1 |

### 关键结构与算法
```rust
// E1 会话：Session{id, path, encoding: Detected(Utf8|Gbk|Latin1), eol, dirty, content: RwLock<String>}
//   编码检测：BOM → UTF-8 校验失败则 chardetng 猜测；保存默认保持原编码 + UI 显示当前编码
// E2 大文件：> 5MB 关闭语法高亮，> 50MB 拒绝编辑器打开仅提供查看器（分片读取）
//   自动保存：脏后 3s 防抖写临时文件（.nforge-autosave），正常保存后清理 —— 崩溃恢复入口
// E4 PDF：操作队列复用 F2 的异步队列；qpdf --split-pages / --pages 合并 / --compress-streams=y
//   压缩结果 > 原文件则回滚保留原文件（幂等保护）
```

### 风险标注
- Monaco worker 与 Tauri CSP：worker 用 blob 加载；`webSecurity` 不放松，文件读写全走 IPC。
- CRLF/LF 混合检测后**整文件统一**，保存前 UI 明示将发生的行尾变更。

## N notes-core 笔记与知识管理

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| N1 | 笔记库模型（Markdown 文件夹为真相源） | S3 |
| N2 | 双链与反链索引 | N1 |
| N3 | 自由画布（tldraw 风格节点图） | N2 |
| N4 | 间隔复习（SM-2 简化版） | N1 |
| N5 | 多存储后端（复用 F6 StorageDriver） | N1 |

### 关键结构与算法
```rust
// N1 真相源 = 磁盘 .md 文件（本地优先）；库根 {appData}/notes 或用户指定目录
//   索引：frontmatter 解析(yaml) + 标题/标签/链接入 SQLite（notes.db，重建可全量恢复）
// N2 链接语法 [[目标|别名]]；索引器正则提取 → links 表(src,dst)；反链查询 JOIN
//   重命名文件 → 全库引用同步改写（先收集再逐文件原子替换，失败回滚已改文件列表）
// N3 画布：节点=笔记引用/便签/图片；边=有向连线；数据 .nforge-canvas.json 与 md 同目录
// N4 SM-2：quality(0-5) → EF' = EF + (0.1 - (5-q)*(0.08+(5-q)*0.02))，EF 下限 1.3；
//   q<3 重置间隔 1 天；队列 = 今天到期卡片
```

### 风险标注
- 用户可能用外部编辑器改文件：fs watcher 全量索引重建兜底（mtime 比较，增量优先）。
- 画布 JSON 与 md 双写一致性：以 md 为准，画布文件损坏时仅丢画布不丢笔记。

## T term-core 终端与运维

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| T1 | ConPTY 封装（ConptyPort → win-integration） | S3 |
| T2 | xterm.js 前端 + 数据管道（背压） | T1 |
| T3 | SSH/SFTP（russh crate） | T1 |
| T4 | 端口转发/跳板机（本地 SOCS5 链） | T3 |
| T5 | WSL 集成（wsl.exe -e 分发检测） | T1 |
| T6 | Docker 管理（命名管道 npipe:////./pipe/docker_engine HTTP） | T3 |

### 关键结构与算法
```rust
// T1 ConptyPort: spawn(cfg: {shell, cwd, env, cols, rows}) -> PtyHandle{write(bytes), resize(c,r), kill(), output: mpsc}
//   实现要点：CreatePseudoConsole + 两个管道对；读线程 → mpsc；关闭顺序：ClosePseudoConsole → 等子进程 → 关句柄
// T2 背压：后端 → 前端 emit 批处理（8ms 窗口合并，单次 ≤ 64KB）；前端 ack 落后 > 4MB 时暂停读取（拉模型降级）
//   终端内容敏感：默认不进日志；用户显式开启"会话记录"才写文件并托盘常显图标
// T3 russh：known_hosts 严格校验（首次指纹确认 UI）；私钥走 vault-core 引用（不复制私钥内容）
// T6 Docker：HTTP over named pipe；容器列表 2s 轮询 + 手动刷新；日志流 tail -f 语义
```

### 风险标注
- ConPTY 句柄泄漏 = 最常见缺陷：PtyHandle 必须 Drop 安全 + 模块 stop 时强制回收全部会话。
- resize 竞态：resize 与 write 并发会损坏转义序列 —— 会话内用顺序队列串行化。
- SSH 主机密钥变更必须强提示（TOFU 破坏告警），不允许静默接受。

## SY sys-core 系统管理

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| SY1 | 包管理器抽象：`PkgManager` trait（winget/scoop/choco 探测与适配） | S3 |
| SY2 | 已装清单合并视图（多源去重，winget 优先） | SY1 |
| SY3 | 系统清理（临时目录/回收站/更新缓存，白名单机制） | SY1 |
| SY4 | 资源监控（CPU/内存/磁盘/网络，uPlot 实时图） | S4 |

### 关键结构与算法
```rust
// SY1 trait PkgManager { available() -> bool; list(); search(q); install(id, confirm: UacPolicy); uninstall(id); upgrade_all() }
//   所有变更操作前弹确认（列出将执行的确切命令行）；命令输出流式回传 UI
// SY3 清理项模型：{ target: PathPattern, safe_default: bool, reclaim_est }; 扫描→汇总→确认→执行
//   白名单：正在运行进程的执行文件、最近 24h 修改文件默认跳过
// SY4 采样：WMI 太慢 —— 用 PDH API（win-integration Port）1s 采样，环形缓冲 300 点；事件节流 1s 推 UI
```

### 风险标注
- winget 输出进度条控制字符：解析走 `--disable-interactivity` + JSON 输出（可用时）。
- 清理操作必须逐项可撤销说明；系统还原点创建（可选，管理员）放在大清理前。

## 阶段三验收

- [ ] 终端 10k 行/秒输出不掉字、不卡 UI；关闭窗口无残留 conhost 子进程
- [ ] 笔记外部修改（VS Code 改 md）10s 内索引同步；重命名引用改写零失败
- [ ] PDF 合并 100 个文件内存峰值 < 300MB
- [ ] 清理扫描预估与实际回收误差 < 10%；误删可从回收站恢复
