# NexusForge 实施总纲（代码级细化索引）

> 版本：v1.0 ｜ 上游依据：[DESIGN.md](./DESIGN.md) ｜ 本文件定义实施顺序、依赖关系与细化文档索引。
> 细化文档位于 `docs/impl/`，每份均按统一模板编写：**目标 → 实现步骤（含依赖编号）→ 代码结构 → 核心算法 → 输入/输出 → 技术细节与潜在问题**。

## 1. 细化文档索引

| 文档 | 模块 | 阶段 | 细化深度 |
|------|------|------|----------|
| [impl/01-host-core.md](./impl/01-host-core.md) | 宿主核心 | 一（P0） | 完整代码级 |
| [impl/02-clipboard-core.md](./impl/02-clipboard-core.md) | 剪切板中枢 | 一（P0） | 完整代码级 |
| [impl/03-screenshot-core.md](./impl/03-screenshot-core.md) | 截图与录屏 | 一（P0） | 完整代码级 |
| [impl/04-ocr-core.md](./impl/04-ocr-core.md) | OCR 与翻译 | 一（P0） | 完整代码级 |
| [impl/05-phase2-modules.md](./impl/05-phase2-modules.md) | proxy / vault / file / desktop / kvm | 二（P1） | 步骤级 + 关键结构 |
| [impl/06-phase3-modules.md](./impl/06-phase3-modules.md) | editor / notes / term / sys | 三（P2） | 步骤级 + 关键结构 |
| [impl/07-phase4-modules.md](./impl/07-phase4-modules.md) | automation / 同步 / 性能 / 发布 | 四 | 步骤级 + 关键结构 |
| [UI-DEMO.md](./UI-DEMO.md) | UI 原型（[demo/index.html](../demo/index.html)） | 阶段一前 | 交互/视觉基准（Fluent 令牌映射） |
| [UI-PLAN.md](./UI-PLAN.md) | 前端开发实施计划 | 阶段一 | U1–U8 任务分解 + M1–M4 里程碑 |

## 2. 实施顺序与依赖图

```
S1 Workspace 骨架 ──► S2 错误体系 ──► S3 Module trait + Ports ──┐
                                                                ├─► S5 模块生命周期管理 ──► S7 Tauri 集成 ──► 阶段一可运行
                     S4 事件总线 ──► S6 配置中心/快捷键/托盘/日志 ─┘                                    │
                                                                                                        ▼
                              C1..C9 剪切板 ─┐  P1..P8 截图 ─┐  O1..O8 OCR ─┐                        阶段二
                              ───────────────┴──────────────┴──────────────┘（C/P/O 可并行，
                                               三者仅依赖 S1–S7 与各自 Port 实现）

阶段二：K1 键鼠共享(TCP/UDP) ──► V1..V7 密码库 ──► F1..F7 文件 ──► PR1..PR6 代理 ──► D1..D4 桌面效率
        （PR 依赖 V 的加密原语复用；PR 的系统代理恢复钩子需回改 S6 崩溃恢复注册点）

阶段三：T1..T5 终端(ConPTY) ──► E1..E4 文本PDF ──► N1..N5 笔记 ──► SY1..SY4 系统管理
阶段四：A1..A6 自动化+WASM ──► SYNC1..SYNC4 同步 ──► PERF1..PERF3 性能 ──► REL1..REL4 发布
```

### 2.1 第一阶段任务序列（严格顺序执行）

| 步骤 | 任务 | 依赖 | 产出 | 验收 |
|------|------|------|------|------|
| S1 | Rust workspace + Tauri 2 壳 + Vite/React 骨架 | 无 | 可 `cargo tauri dev` 启动空窗口 | 空窗口 + HMR 正常 |
| S2 | AppError/ModuleError 体系 | S1 | host-core/src/error.rs | 单测通过 |
| S3 | Module trait、ModuleContext、Port traits | S2 | module.rs、ports.rs | 编译通过 |
| S4 | 事件总线（主题注册表 + 背压） | S2 | events.rs | 并发发布单测 |
| S5 | ModuleRegistry + 生命周期 + panic 隔离 | S3,S4 | registry.rs、lifecycle.rs | 崩溃隔离集成测试 |
| S6 | 配置中心/快捷键/托盘/日志/崩溃恢复 | S5 | config.rs 等 | 配置迁移单测 |
| S7 | Tauri Plugin 集成 + IPC 注册宏 | S5,S6 | plugin.rs、ipc.rs | 前端可 invoke |
| C1–C9 | 剪切板中枢全量 | S1–S7 | clipboard-core | 见 02 文档验收 |
| P1–P8 | 截图录屏全量 | S1–S7 | screenshot-core | 见 03 文档验收 |
| O1–O8 | OCR 翻译全量 | S1–S7 | ocr-core | 见 04 文档验收 |

**并行规则**：C/P/O 三条线互不依赖，可三人并行；每条线完成即合入主干，不得交叉修改 host-core（如需新增 Port，先在 host-core 提 PR）。

### 2.2 通用编码规约（所有细化文档默认遵循）

1. 模块 crate 禁止直接依赖 `windows` crate —— 只能调用 host-core 的 Port trait（DESIGN.md O2）。
2. 模块间禁止互调，只允许 `EventBus` 订阅（O1）；主题必须先在 `TOPIC_REGISTRY` 注册。
3. 所有 IPC 命令返回 `Result<T, AppError>`，错误码 `{MODULE}_{CATEGORY}_{NNN}`（O7）。
4. SQL 一律参数化；查询必须带 LIMIT；FTS5 用外部内容表 + 触发器同步（DESIGN §4.1）。
5. 文件写入一律"临时文件 + 原子 rename"；删除敏感数据先覆写（§8）。
6. 注释与文档字符串用中文；标识符用英文。
7. 每个模块 crate 的测试不依赖真实系统调用 —— Port 一律 mock。

## 3. 验收总闸（阶段一出口条件）

- [ ] 启动 < 1.5s（release，含全部 P0 模块）
- [ ] 复制 → 历史可见延迟 < 100ms（P95）
- [ ] 截图快捷键 → 选区层出现 < 200ms
- [ ] FTS 搜索 5000 条 < 50ms
- [ ] 内存基线 < 250MB（空闲 10 分钟）
- [ ] 任意 P0 模块 panic 不导致宿主退出，UI 可重启该模块
- [ ] CI（rustfmt/clippy/test/基准）全绿
