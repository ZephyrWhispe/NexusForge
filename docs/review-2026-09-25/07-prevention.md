# 07 · 预防措施与整改批次

> 本文汇总"如何避免同类问题再次发生"，并把 99 条发现编排为可执行的整改批次。
> 目标读者：维护者 / 评审者 / 后续参与开发的 AI 与人类。

---

## 1. 根因归纳（从 99 条问题反推）

把 `01-inventory.md` 的问题按"失效机制"聚类，可归为 **8 个根因**。修单点问题只能解决当下，修正根因才能防复发。

| # | 根因 | 典型问题 | 频次 |
|---|---|---|---|
| R1 | **边界信任假设未显式化**：跨信任边界（提权 / 对端设备 / 远端服务器 / 用户可写目录 / 插件）的输入被当作可信 | `SEC-02`（提权 args 透传 + 用户可写 catalog）、`SEC-06`（远端文件名）、`SEC-07`（跨设备笔记）、`SEC-13`（全局剪贴板格式）、`SEC-16`（插件 open_url）、`SEC-15`（远端包名） | 6+ |
| R2 | **路径 = 安全边界，但校验写法不正确**：字符串 `contains("..")`/`is_absolute()` 在 Windows 语义下不充分；多处裸 `root.join(user_input)` | `SEC-03`、`SEC-04`、`SEC-05`、`SEC-06`、`SEC-17` | 5 |
| R3 | **"失败/损坏"被静默处理**：吞错、`.ok()`、`filter_map(ok)`、降级为默认值 | `COR-08`（游标照推）、`COR-20`（计数谎报）、`COR-21`（入库吞错）、`COR-23`（恢复记录/画布静默）、`STD-01`（空 catch） | 5+ |
| R4 | **非原子写 / 无回滚**：就地覆盖、先删后改名、批量操作无回滚 | `COR-04`（编辑器）、`COR-11`（vault header）、`COR-19`（桌面整理）、`COR-22`（无事务） | 4 |
| R5 | **信任根 fail-open**：不可解析即重建，而非拒绝 | `COR-05`（设备身份覆盖重生） | 1（但后果最重） |
| R6 | **生命周期/并发约定未闭环**：spawn 无归属、取消信号不复位、回调内加锁、async 内阻塞 IO | `COR-06`、`COR-07`、`COR-30`、`PERF-01`、`PERF-07`、`PERF-08` | 6 |
| R7 | **密码学与协议细节未按"必须满足的性质"审视**：只做 roundtrip 测试，不验证 nonce 唯一性/方向分离/重放 | `SEC-01`（P0）、`SEC-11`（配对抢占）、`COR-03`（锁页错地址） | 3 |
| R8 | **文档与事实分离**：裁决改了正文没改、接口清单未随代码更新、验收标准不可执行 | `DOC-01`…`DOC-14`、`GOV-02`、`GOV-03`、`GOV-08` | 14+ |

> **观察**：R1/R2/R3/R4/R5 属于"安全与数据完整性的编码范式"问题——它们**最需要工具与模板**，而不是更多人工 review；R6/R7 需要**覆盖负例的测试模板**；R8 需要**自动比对**。

---

## 2. 十二条机制建议（含落地位置与验收方式）

| ID | 措施 | 落地位置 | 覆盖问题 | 验收方式 |
|---|---|---|---|---|
| **P-01** | **路径唯一入口 `resolve_in_root`**：所有"用户/远端给的相对路径"只能经此函数（`Component` 级校验 + `starts_with(root)` + 可选 `canonicalize`）；并建立 `safe_rel_path` 供归档/远端落点/命名模板复用 | `host-core::fs` + 各自 crate 薄封装 | `SEC-03/04/05/06/17`、`COR-16` | 源码扫描：`root.join(` 后不出现用户输入；每域 mock 负例测试（`..`、`C:`、`\Windows`、UNC、ADS） |
| **P-02** | **原子写唯一入口 `write_atomic`**（同目录 tmp + `sync_all` + rename；失败清 tmp），禁止"rename 前 remove_file" | `host-core::fs` | `COR-04/11`、`SEC-09`、`SEC-17` 相关 | 源码扫描：生产代码 `std::fs::write(` 仅出现在该工具内；"保存失败不掉数据"回归测试 |
| **P-03** | **提权输入只允许枚举/ID**：跨提权边界的 IPC 不接受字符串参数与路径；helper 侧用编译期常量模板表；外置可配置目录禁止覆盖内置项、必要时加签名校验 | `nexusforge-helper` + `win-integration` + `sys-core::winops` | `SEC-02` | 负例测试：任意 `args` 拒绝、越界 `registry.key` 拒绝、外置覆盖内置拒绝；源码扫描 `args.to_vec()` 无命中 |
| **P-04** | **AEAD/协议"性质测试"模板**：任何 AEAD 使用必须证明 (key, nonce) 全域唯一 + 方向分离 + 重放拒绝；协议升级走版本协商 | `host-core::wire` + 协议测试 | `SEC-01`、`SEC-11` | 三条测试必存（方向密钥必异 / 同明文同序号密文必异 / 重放必拒）；PR 模板勾选项 |
| **P-05** | **"失败不可静默"落地为类型**：数据类操作返回 `Result` 并带 `AppError{code,hint}`；禁止对写操作使用 `.ok()`/`let _ =`；统计类操作返回"成功/失败明细" | 全 crate + `commands/*` | `COR-08/20/21/22/23`、`STD-01` | 源码扫描 + 行为测试（注入失败 → 断言返回 Err/明细且 UI 可见） |
| **P-06** | **信任根 fail-closed 模板**：`match read { NotFound => create, Ok(bytes) => parse_or_fail, Err(e) => fail }`；不可解析时留证（`.corrupt`）并拒绝覆盖 | `host-core::device`、`sys-core`、`crates/*` 的 state 文件 | `COR-05`、`COR-23` | 每个信任根文件一条负例测试（不可解 → 拒绝且原文件保留） |
| **P-07** | **生命周期闭环模板**：`CancellationToken`（或代际令牌）替代裸 `AtomicBool`；spawn handle 必须存字段并在 stop/Drop 中 abort/join；启动/停止幂等 | `host-core` 工具 + 所有模块 | `COR-06/07/30`、`COR-14` | `start→stop→start` 行为测试（含"事件流恢复"断言）；handle 泄漏计数断言 |
| **P-08** | **热路径（回调/钩子/中断）零锁零 IO 零分配**：状态以 `ArcSwap` 不可变快照发布；回调只做判定与投递 | `win-integration` + 调用方（`kvm-core` 等） | `PERF-01`、`PERF-07/08` | 回调路径压测 + "无锁断言"；`std::thread::sleep`/同步 IO 在 async 中的扫描豁免清单 |
| **P-09** | **统一预算表（输入上限）**：图像字节/像素、压缩包条目/单件/总量、文本长度、列表 `limit`、下载字节、guest 字符串长度 | `host-core::limits` + 各 IPC 边界 | `COR-02/24/25`、`PERF-05`、`SEC-10` | 每类预算一条"超限即拒"测试；`limit` 统一 `clamp_limit` |
| **P-10** | **IPC 契约单一事实源**：命令表数据驱动（或从 `commands/*` 生成 `ipc_contract.json`），据此自动生成 `permissions/*.toml` 与前端 DTO，并做三方一致性断言 | `src-tauri` + `src/ipc` | `GOV-09`、`DOC-01/10`、`TEST-02` | CI 集合差集为空；DESIGN §6 命令名 ⊆ 注册表 |
| **P-11** | **破坏性操作制度化**：维护"破坏性操作清单"（操作 / 确认方式 / 可否撤销 / 测试名），新增操作必须同步更新清单 | `CONTRIBUTING.md` + 前端面板 | `COR-15`、`COR-26`、`COR-27`、前次 U6 家族 | 清单与代码同 PR；每条至少有"取消路径"测试 |
| **P-12** | **文档可验证性**：验收标准必须指向可执行命令/测试文件；枚举型事实引用权威源；裁决入库时同步更新正文；文档锚点与命令名做自动比对 | `docs/*` + CI 脚本 | `DOC-01`…`DOC-14`、`GOV-02/03/08` | 文档核对脚本（命令存在性、锚点、决策号完整性、模块数/命令数一致性） |

---

## 3. 门禁清单（CI 增补，可直接照抄）

```yaml
# .github/workflows/ci.yml —— 建议增补（在现有 rust / frontend 两个 job 基础上）
  rust:
    steps:
      # …既有：fmt check / clippy -D warnings / cargo test / bench --no-run
      - name: cargo-deny (licenses / advisories / bans)      # GOV-02
        uses: EmbarkStudios/cargo-deny-action@v2
      - name: 源码安全范式断言                                # P-01/P-02/P-03/P-05/P-08
        shell: pwsh
        run: ./tools/assert-patterns.ps1
      - name: 文档-代码一致性核对                              # P-12/DOC-*
        shell: pwsh
        run: ./tools/check-docs-consistency.ps1

  frontend:
    steps:
      # …既有：tsc --noEmit / eslint / vitest
      - name: npm audit (high)                                # GOV-02
        run: npm audit --audit-level=high
      - name: 产物体积预算                                     # PERF-03
        run: npm run build && node tools/check-bundle-size.mjs
```

`tools/assert-patterns.ps1` 建议断言的模式（每条都对应本次发现，成本极低）：

| 断言 | 对应问题 |
|---|---|
| `crates/**` 生产代码不出现 `std::fs::write(`（除 `write_atomic` 定义处与测试） | `COR-04/11` |
| `crates/notes-core`、`screenshot-core`、`file-core` 不出现 `root.join(` 直连用户输入形态 | `SEC-03/04/05/06/17` |
| `crates/win-integration/src/maintenance.rs` 不出现 `args.to_vec()` | `SEC-02` |
| `src-tauri/src/commands/*.rs` 不出现裸 `limit.unwrap_or` | `COR-25` |
| 生产代码不出现 `catch` 空块 / `.catch(() => {})`（ESLint 侧） | `STD-01` |
| `events.rs` 每个注册主题都能找到发布者与订阅者，或列于"预留清单" | `STD-10`、`PERF-10` |
| `typescript` 侧 `dangerouslySetInnerHTML` 仅出现在 `MarkdownView.tsx` | `SEC-07` |

`tools/check-docs-consistency.ps1` 建议断言的项：`DESIGN §6` 命令名 ⊆ 注册表；各 `docs/*.md` 的 `§x.y` 锚点可解析；`DECISIONS.md §1` 覆盖全部 D-xx；`README` 的模块数 == `MODULES.length`；文档中 `cargo test -p <crate>` 的 crate 存在。

---

## 4. PR 检查清单（建议写入 `.github/pull_request_template.md` 与 `CONTRIBUTING.md`）

**安全（任一项为"是"时必须说明）**
- [ ] 改动是否引入了新的**跨信任边界输入**（提权/对端/远端/用户可写文件/插件）？→ 输入白名单与负例测试（`P-03`）
- [ ] 是否处理**路径**？→ 经 `resolve_in_root`/`safe_rel_path`，含 `..`/绝对/盘符/UNC/ADS 负例（`P-01`）
- [ ] 是否使用**密码学**（AEAD/KDF/签名/nonce）？→ 性质测试（唯一性/方向/重放）（`P-04`）
- [ ] 是否修改**CSP / capabilities / permissions / assetProtocol**？→ 同步 `security_config.rs` 断言（`SEC-08/14`，`GOV-09`）
- [ ] 是否新增**宿主函数/插件能力**？→ 授权范围有文档与负例（`SEC-16`）

**数据完整性**
- [ ] 是否**写文件/写库**？→ 原子写 + 事务 + 失败可见（`P-02`/`P-05`）
- [ ] 是否涉及**信任根文件**（identity/paired/known_hosts/state）？→ fail-closed + 留证（`P-06`）
- [ ] 是否有**批量操作**？→ 预登记/回滚/报告（`COR-19/22`）
- [ ] 是否有**用户可感知的破坏性操作**？→ 更新破坏性清单 + 取消路径测试（`P-11`）

**正确性与并发**
- [ ] `spawn`/`thread::spawn` 的 handle 是否归属字段并在 stop 收回？→ `P-07`
- [ ] `start/stop` 是否幂等、取消信号是否复位？→ `P-07`
- [ ] 热路径（钩子/回调/中断）是否零锁零 IO？（`P-08`）
- [ ] 异步上下文中是否有阻塞调用？（`PERF-07/08`）

**性能与规范**
- [ ] 新增/修改的**输入**是否有上限（`P-09`）？
- [ ] 长列表是否虚拟化？大依赖是否分包？（`PERF-03/04`）
- [ ] 是否新增字面量颜色/magic px/超长文件？（`STD-04/05`）
- [ ] 错误是否带 `code` + `hint`（`DESIGN §8.1`）？

**文档**
- [ ] 本次改动是否使某份文档失真（接口清单/状态/验收标准）？→ 同步更新（`P-12`）

---

## 5. 破坏性操作清单（制度化模板，来自本次审查实测）

建议在 `CONTRIBUTING.md` 维护下表（**新增操作必须补行**）。当前状态（✅ 有确认 / ⚠️ 部分 / ❌ 无）：

| 模块 | 操作 | 确认 | 可否撤销 |
|---|---|---|---|
| vault | 删除条目 / 删除文件夹 / 丢弃未保存编辑 | ✅ | 否（已明示无回收站） |
| vault | 关闭 Windows Hello 免密 | ⚠️ 仅通知 | 可再启用 |
| clipboard | 删除行 / 清空（保留置顶）/ 清空栈 / 删除分组 / 导入备份 | ✅ | 否（部分语义可回退） |
| clipboard | 揭示明文 / 永不入库 | ✅ | 可收起 |
| file | 删除（回收站）/ 全部覆盖 / 压缩（可能覆盖）/ 解压 | ✅ | 回收站可还原；覆盖不可 |
| file | 应用批量重命名 | ⚠️ 预览+勾选流（无二次确认） | 否 |
| file | 丢弃崩溃恢复记录 | ❌ | — |
| kvm | 解除配对 / 发送文件 | ✅ | 需重新配对 |
| term | 结束会话 / 删除指纹 / 停止容器 / SFTP 删除 | ✅ | 容器可重启 |
| automation | 删除规则 / 清空死信 / 删除插件 / 批量重放 | ✅ | 否 |
| proxy | 删除订阅 / 切换内核 / 安装资产 / 应用预设 | ✅ | 部分 |
| screenshot | 删除记录（连带磁盘文件） | ✅ | 否 |
| desktop | 删除随记 / 一键整理 / 还原 | ✅ | 整理可还原 |
| desktop | **回退内置六类（清自定义映射）** | ❌ → 见 `COR-27` | 否 |
| sys | 执行清理（可选回收站）/ 结束进程（复述名）/ 包卸载升级 / tweak 回滚 | ✅ | 清理可回收站 |
| notes | 删除笔记 / 重命名 / 删除卡片 / 删除画布节点 | ✅ | 否 |
| editor | 关闭脏缓冲 / 压缩 / 水印 / 有损编码切换 | ✅ | 否 |
| editor | **另存为覆盖已存在文件** | ❌ → 见 `COR-27` | 否 |
| notes | **切换/新建笔记丢弃未保存内容** | ❌ → 见 `COR-15` | 否 |
| sync | 以本地副本重新生效并推送 | ✅ | 否 |

> 结论：前次报告点名的"删除类无确认"**已全部补齐**；新缺口集中在**"隐含覆盖 / 丢弃 / 重置"型**操作（`COR-15`/`COR-27`）与"属性写回 / 无二次确认的批量操作"。清单化的价值正在于——把"哪些操作必须确认"从个人判断变为可核对的表格。

---

## 6. 整改批次（严格顺序，含验收）

### 批次 A · 热修（当天，P0）
| 项 | 内容 | 验收 |
|---|---|---|
| A1 | `SEC-01` 会话 AEAD 方向化密钥 + 接收序号校验（KVM + sync 同改） | 三条性质测试通过；`cargo test --workspace` 全绿；协议版本升 v2 并拒 v1 对端（带 hint） |
| A2 | 登记 `DECISIONS.md`（新条目）+ 影响面说明（KVM/sync 会话机密性与完整性） | 决策入册 |

### 批次 B · P1 安全（本周）
`SEC-02`（提权参数面 + catalog 覆盖）→ `SEC-03/04/05/06`（四处路径，建议一次把 `resolve_in_root`/`safe_rel_path` 铺开）→ `SEC-07`（DOMPurify + `MarkdownView`）→ `SEC-08`（asset scope 收窄 + 断言）→ `SEC-09`（代理还原不变式 + 崩溃恢复测试）。

**验收**：每个 ID 一条"先失败后通过"的负例测试；`security_config.rs` 增补 asset/CSP 断言；路径类加恶意输入夹具。

### 批次 C · P1 数据完整性（本周）
`COR-01`（blob 读回 + 预览）→ `COR-02`（WASM 分配次序）→ `COR-03`（密钥堆化 + 锁页配对）→ `COR-04`（`write_atomic` 铺开）→ `COR-05`（身份 fail-closed）→ `COR-06/07`（取消令牌 + handle 归属）→ `COR-08`（游标不越失败项）→ `COR-09`（编码闭环）。

**验收**：数据类测试断言"副作用"（磁盘/DB 内容）；`start→stop→start` 行为测试；保存失败不掉数据用例。

### 批次 D · P2（按 `01-inventory.md` 顺序，分批提交）
安全 P2（`SEC-10`…`SEC-19`）→ 正确性 P2（`COR-10`…`COR-27`）→ 性能 P2（`PERF-03`…`PERF-09`）→ 规范 P2（`STD-01`…`STD-04`、`GOV-01`…`GOV-07`）→ 文档 P2（`DOC-01`…`DOC-04`）。

**验收**：每批 `cargo test` + `clippy -D warnings` + 前端 `tsc/lint/test` 全绿；每批至少补 1 个门禁断言（`P-01`…`P-12` 中对应项）。

### 批次 E · 治理一次性补齐
`GOV-01` rustfmt.toml → `GOV-02` deny.toml + CI 步骤 → `GOV-03` dependabot → `GOV-04` SECURITY/CODEOWNERS/CONTRIBUTING → `GOV-05` gitattributes/editorconfig → `GOV-06` MSRV + `[workspace.lints]` + 元数据 → `GOV-07` 工作区清理 → `GOV-08/09` 基准与契约门禁。

**验收**：CI 绿灯且新增步骤**在本仓库真实执行过**（不接受"配置了但跑不通"）；`contributing` 文档包含第 4/5 节清单。

### 批次 F · P3 与文档
`STD-05`…`STD-10`、`COR-28`…`COR-31`、`PERF-10`、`DOC-05`…`DOC-14`、`TEST-01`…`TEST-03`。

**验收**：文档核对脚本全绿；P3 项可合并提交但需逐条勾选。

---

## 7. 度量看板（建议纳入每轮审查，观察趋势）

| 指标 | 本次值 | 目标 |
|---|---|---|
| `cargo clippy --workspace --all-targets` 告警数 | **0** | 保持 0 |
| `cargo test --workspace` 通过/失败 | **985 / 0**（4 ignored） | 失败恒为 0；ignored ≤ 5 且逐条登记理由 |
| 生产代码 `unsafe` 缺 SAFETY 注释 | ≥ 2 处（`dpapi.rs`、`usn.rs`） | 0（由 `undocumented_unsafe_blocks = deny` 保证） |
| 非测试路径 `unwrap/expect/panic` | 9 处 | ≤ 3（其余均有证明性注释） |
| 静默吞错（空 catch / `.ok()` 于写操作 / `let _ =` 于 IO） | 空 catch 2 + 其他 40+ | 逐类 ≤ 3 且均带理由注释 |
| 提权边界字符串参数 | 1 处（`args` 透传） | 0 |
| 裸 `root.join(<用户输入>)` | 5+ 处 | 0 |
| IPC 契约测试覆盖 | 4 / 255 | ≥ 命名与签名一致性（集合差集） |
| 文档锚点/命令名可解析率 | 悬空 4 + 不存在命令 3 | 100% |
| 前端 magic px / 内联 style | 552 / 168 | 只减不增（新代码 0 新增） |
| `THIRD_PARTY_LICENSES` 与实际依赖 | 手动生成（无门禁） | 由 `cargo-deny` 门禁覆盖 |

---

## 8. 一句话总结

本次 99 条问题中，**真正的新增风险集中在两处**：一是 `SEC-01` 这类"看似正确、性质不成立"的密码学误用（唯一 P0），二是 `SEC-02` 这类"边界信任假设未显式化"的提权面问题。前次报告的质量提升（clippy 0 告警、985 测试全绿、权限台账一致、破坏性操作确认补齐）是真实的，但**"测试全绿 + 清单勾完"不等于"性质正确"**——因此本报告把重心放在"把性质写成可执行断言"（`P-01`…`P-12`），而不是再列一批需要人工记住的注意事项。