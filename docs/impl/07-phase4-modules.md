# 07 第四阶段细化（automation / 同步 / 性能 / 发布）

> 依赖：阶段三出口 ｜ 细化深度：步骤级 + 关键结构 + 风险标注

## A automation-core 自动化与拓展

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| A1 | 规则模型（条件-动作 DAG） | S4 |
| A2 | 条件求值器（事件订阅 + 表达式） | A1 |
| A3 | 动作执行器（复用各模块 IPC 语义） | A1 |
| A4 | Task Scheduler 集成（定时/开机触发） | A2 |
| A5 | WASM 插件运行时（wasmtime） | A3 |
| A6 | 插件管理器 + 市场客户端 | A5 |

### 关键结构与算法
```rust
// A1 Rule { id, on: Trigger, when: Option<Expr>, then: Vec<Action>, cooldown, enabled }
//   Trigger::{Event{topic, filter}, Hotkey, Schedule(cron), Startup}
//   Expr：受限表达式（JSONPath 取值 + 比较/逻辑，禁止任意代码）
// A3 Action::{IpcCommand{module, cmd, args}, Notify, RunScript(wasm), OpenUrl}
//   执行语义：单规则内串行；规则间并行；Action 失败按 retry(2, 指数) 后进入死信面板（UI 可查/重放）
// A5 wasmtime：Store 限内存 64MB、fuel 计算限额（防死循环）、无网络/文件系统 WASI；
//   宿主函数白名单注入（仅 open/notify/log 三个能力）
// A6 manifest: {id, name, version, api_version, permissions[], sha256}
//   加载校验：api_version 兼容矩阵 + 签名（市场分发带签名）+ 沙箱权限逐一映射到注入的宿主函数
```

### 风险标注
- 规则风暴（事件触发规则又产生事件）：执行图带深度上限 3 + 全局冷却表；超限规则自动禁用并告警。
- Task Scheduler 权限：注册任务以当前用户运行；需要最高权限的任务明确标 UAC 盾图标。

## SYNC 跨设备同步

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| SYNC1 | 同步拓扑（局域网 P2P 优先，中继可选自建） | K3 |
| SYNC2 | 数据集同步协议（剪贴板/待办/笔记变更流） | SYNC1 |
| SYNC3 | 冲突解决（LWW + 字段级合并） | SYNC2 |
| SYNC4 | 端到端加密（复用 K2 配对信任根） | SYNC1 |

### 关键结构
```rust
// SYNC2 变更流：op_log { op_id(ULID), entity, entity_id, field, value_enc, ts, device } 追加表
//   同步 = 交换 op_log 游标 → 拉取缺失 → 本地应用
// SYNC3 冲突：默认 LWW(ts, device_id 破平)；文本字段尝试 3-way 合并（diff-match-patch）失败则保留双版本并通知
```

### 风险标注
- 密码库条目**永不**自动同步（独立于 SYNC2，仅手动导出加密包）。
- "仅局域网"模式硬开关：中继地址清空 + 防火墙出站规则提示。

## PERF 性能优化

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| PERF1 | 启动路径剖析 + 优化（目标 < 1.5s） | — |
| PERF2 | 内存优化（release profile + 数据冷热分层） | — |
| PERF3 | 前端性能（代码分割/虚拟列表/缓存） | — |

### 关键要点
```toml
# PERF2 Cargo.toml release
[profile.release]
lto = "fat"          # 链接期全优化
codegen-units = 1
panic = "abort"      # 注意：与模块 panic 隔离不冲突（隔离在 spawn 层 catch join error，abort 仅影响主进程 panic）
strip = true
```
- 冷热分层：剪切板 > 7 天未访问条目不加载 blob（懒加载已具备）；索引内存用紧凑结构。
- 前端：模块路由级 React.lazy；首屏只加载宿主框架 + 上次活跃模块。

### 风险标注
- `panic="abort"` 后 `catch_unwind` 失效——确认所有隔离都在 tokio join 层（spawn 返回 Err(is_panic)），主线 panic 语义不变。

## REL 发布 1.0

### 步骤
| # | 任务 | 依赖 |
|---|------|------|
| REL1 | 自动更新（Tauri updater + 签名密钥管理） | — |
| REL2 | 安装包（NSIS/MSI 双格式 + 卸载清理） | — |
| REL3 | 代码签名（SignPath Foundation 或自签过渡） | REL2 |
| REL4 | 分发渠道（GitHub Releases → winget/Scoop manifest） | REL3 |

### 关键要点
- 更新签名私钥**仅存 CI Secret**；`latest.json` 附签名；更新前校验双签名（Tauri + 包签名）。
- NSIS：卸载钩子清理 `{appData}`（询问保留用户数据，默认保留）；还原系统代理残留。
- winget manifest 提交 PR 到 microsoft/winget-pkgs；版本升级同步 sha256。

### 风险标注
- 更新失败回滚：安装器支持旧版静默重装参数；更新服务端 latest.json 校验失败 → 保持当前版本并告警。

## 阶段四验收（1.0 发布闸）

- [ ] 冷启动 < 1.5s；空闲内存 < 250MB；FTS < 50ms（性能回归 CI 连续 7 天绿）
- [ ] WASM 插件：恶意样本（无限循环/内存炸弹）被 fuel/内存限额终止，宿主无感
- [ ] 同步：两设备 10 分钟双向变更收敛，冲突文档保留双版本
- [ ] 更新：从 0.9 → 1.0 灰度升级 + 签名校验 + 卸载清理全通过
- [ ] 发布检查清单（01 文档模板）全部勾选，签名验证 `signtool verify /pa` 通过
