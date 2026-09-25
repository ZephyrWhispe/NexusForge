# NexusForge 全量审查报告（2026-09-25）

> 审查日期：2026-09-25 ｜ 对象：`master` 工作区（含未提交改动）
> 规范基线：[DESIGN.md](../DESIGN.md) v1.0（含 §11 修订登记）、[IMPLEMENTATION.md](../IMPLEMENTATION.md)、[DECISIONS.md](../DECISIONS.md)（D-01…D-33）
> 前次审查：[REVIEW-2026-09-18.md](../REVIEW-2026-09-18.md)（本文档对其"已修项"逐条复核，见 §5）
> 本目录：审查结果独立存放，不修改任何产品代码。

---

## 1. 如何使用本报告

| 文件 | 内容 | 读者 |
|------|------|------|
| [README.md](./README.md) | 方法论、实测基线、统计与评分、复核矩阵 | 全体 / 决策者 |
| [01-inventory.md](./01-inventory.md) | **问题分类清单（按严重程度排序）**，含 ID / 位置 / 类型 / 影响 | 全体 |
| [02-security.md](./02-security.md) | 安全类问题详解：描述 → 解决方案（步骤+代码）→ 修复前后对比 → 预防措施 | 开发 / 安全 |
| [03-correctness.md](./03-correctness.md) | 正确性与数据完整性问题详解（同上四段结构） | 开发 |
| [04-performance.md](./04-performance.md) | 性能问题详解（同上四段结构） | 开发 |
| [05-standards.md](./05-standards.md) | 代码规范与工程治理问题详解 | 开发 / 维护者 |
| [06-docs-inconsistency.md](./06-docs-inconsistency.md) | 文档错误与文档-代码不一致 | 文档 / 全体 |
| [07-prevention.md](./07-prevention.md) | 预防同类问题的机制建议、门禁清单、整改批次 | 维护者 |
| `_raw/` | 本次实测原始日志（clippy / cargo test） | 复核用 |

**ID 规则**：`SEC-`安全 / `COR-`正确性 / `PERF-`性能 / `STD-`规范 / `GOV-`工程治理 / `DOC-`文档 / `TEST-`测试质量。
**严重度**：`P0` 阻断（须立即修，含不可逆数据/机密性风险）、`P1` 严重、`P2` 中等、`P3` 轻微。
**证据分级**：`[实测]` 本报告作者在本机执行命令或逐行读码确认；`[走查]` 代码阅读推理，未动态复现；`[框架]` 依赖框架/OS 语义推理。

---

## 2. 实测基线（2026-09-25）

| 验证项 | 命令 | 结果 |
|--------|------|------|
| Rust lint | `cargo clippy --workspace --all-targets` | **0 告警，EXIT=0** `[实测]`（日志 `_raw/clippy.txt`） |
| Rust 测试 | `cargo test --workspace` | **985 通过 / 0 失败 / 4 忽略，47 个测试二进制，EXIT=0** `[实测]`（日志 `_raw/cargo-test.txt`） |
| 仓库规模 | 目录统计 | `crates/` 192 文件 79,198 行；`src-tauri/src` 21 文件 6,603 行；`src/` 191 文件 41,261 行；`docs/` 37 文件 5,143 行 `[实测]` |
| IPC 命令面 | 脚本去重 | `#[tauri::command]` **255 个**，243 个返回 `Result<_, AppError>`，0 个返回裸 `Result<_, String>` `[实测]` |
| 权限台账 | 集合差集 | `permissions/*.toml` 255 条 `[[permission]]` 与 255 个命令名**双向差集为空**；6 个 capability 引用均已定义 `[实测]` |

**未执行项（审查限制，务必知悉）**：

- `npx tsc --noEmit` / `npm run lint` / `npm run test` / `vite build`：审查会话中 `node` 不在 PATH（已定位 `C:\Program Files\nodejs\node.exe` 但按只读纪律未执行前端构建与测试）。前端类型/lint/vitest 结论均为静态走查。
- `cargo fmt --all --check`：未执行；且仓库无 `rustfmt.toml`（见 `GOV-01`）。
- `cargo deny` / `cargo audit` / `npm audit`：未安装/未执行（见 `GOV-02`）。
- **未做真机交互验收**：KVM 键鼠接管、输入钩子时序、代理崩溃还原、ConPTY 渲染、多屏混合 DPI 等均需真机复现（相关结论已标注 `[走查]`/`[框架]`）。

---

## 3. 审查方法

1. **分片通读**：按依赖与信任边界把 18 个 crate + 前端 10 个子目录 + 文档拆成 8 个工作流，全部逐文件通读（非采样）。
2. **交叉验证**：每个工作流返回的 `P0`/`P1` 结论，由本报告作者**逐条打开源码复核行号与语义**（见 §5 复核矩阵），未通过复核的不予收录或降级。
3. **双视角**：既用项目自定硬约束（事件总线隔离、Windows API 收敛、独立 SQLite、>64KB 走 blob、AppError 错误码、helper 路径白名单）判定"违规"，也用通用工程标准判定缺陷。
4. **与前次报告对齐**：对 REVIEW-2026-09-18 的全部"已修项"逐条复核，避免重复计费与虚假闭环。

---

## 4. 结论总览

### 4.1 严重度分布（本次共 99 条）

| 严重度 | 数量 | 类别分布 |
|--------|------|----------|
| **P0** | 1 | 安全 1 |
| **P1** | 19 | 安全 8 · 功能缺陷/数据完整性 9 · 性能 2 |
| **P2** | 50 | 安全 10 · 功能缺陷 18 · 性能 7 · 规范 4 · 治理 7 · 文档 4 |
| **P3** | 29 | 文档 10 · 规范 12 · 功能缺陷 4 · 测试 3 · 治理 2 · 安全 3（规范类）· 性能 1 |

### 4.2 三个最高优先事项

1. **`SEC-01`（P0）会话 AEAD 双向复用同一密钥与 nonce 计数器** —— ChaCha20-Poly1305 在 KVM 与同步会话中两个方向用同一 key、各自计数器从 0 起，nonce 空间完全重叠（`[实测]`）。后果是机密性被 `C1⊕C2` 直接击穿、可伪造帧、可重放。这是本次审查唯一 P0，须单独热修并回归。
2. **`SEC-02`（P1）提权 Helper 的 `exec` 参数逐字透传 + 外置 catalog 位于用户可写目录** —— 一次 UAC 之后可执行任意参数（`netsh`/`dism`/`sfc`/`powercfg`）与任意 HKLM 写/服务控制（`[实测]` 代码路径，`[框架]` 可达性）。提权边界的"可信输入"假设被打破。
3. **`SEC-03`/`SEC-04`/`SEC-05`/`SEC-06`（P1）四处路径穿越** —— 笔记画布、笔记库（盘符前缀）、解压（根相对条目 `\Windows\x`）、远端文件名（`..`）四条链在 Windows 语义下均可逃出预期根目录写/删任意文件（`[实测]` 代码 + `[框架]` `PathBuf::join` 语义）。

### 4.3 与前次报告的关系（复核结论）

前次报告点名的重灾区**大部分确已修好**（死按钮、静默吞错主体、虚拟列表行高、键盘可达、错误通道、工程门禁首批、破坏性操作确认、剪贴板信封加密、模块重启对称性的一批修复）。**仍未修**的有 4 项：

| 前次条目 | 现状 | 证据 |
|---|---|---|
| U5 Mica 无能力探测 | ❌ 未修 | [MicaBackdrop.tsx:29-35](../../src/layout/MicaBackdrop.tsx#L29-L35) 仍 `if (IN_TAURI) return null` |
| U7 设置每键落盘 | ❌ 未修 | [SchemaForm.tsx:118-124](../../src/settings/SchemaForm.tsx#L118-L124) 仍每 `onChange` 一次 `hostConfigSet` |
| M10 Monaco 全量入口/无 `manualChunks` | ❌ 未修 | [setup.ts:6](../../src/monaco/setup.ts#L6)、`vite.config.ts` 无 `build.rollupOptions` |
| M13 `marked` 输出未净化 | ❌ 未修 | [NotesPanel.tsx:988](../../src/modules/notes/NotesPanel.tsx#L988)、[EditorPanel.tsx:676](../../src/modules/editor/EditorPanel.tsx#L676) |

同时，本次**新发现**了前次报告未覆盖的一批更严重问题（`SEC-01`…`SEC-08`、`COR-01`…`COR-09`），说明"确认已修项"与"全面安全"是两件事——这也是 `07-prevention.md` 主张把安全评审从"清单核对"升级为"威胁建模 + 负例测试"的原因。

### 4.4 维度评分

| 维度 | 评分 | 依据 |
|---|---|---|
| 架构骨架 | **A-** | 模块→模块生产依赖为零（`[实测]` 差集）；Windows API 严格收敛在 `win-integration`；每模块独立 SQLite + WAL；port trait 真实消费 |
| 工程门禁 | **B+** | CI 有 fmt/clippy(-D warnings)/test/bench 编译 + 前端 tsc/eslint/vitest；`security_config.rs`（76KB）为真断言且带负例。缺 rustfmt.toml、cargo-deny、dependabot、MSRV |
| 代码质量 | **B** | clippy 0 告警、`any`/`@ts-ignore`/`console` 前端零命中、错误码体系基本统一；但存在"吞错掩盖损坏"约 10 处、`unsafe` 伪造 `'static` 1 处、`unwrap` 在非测试路径 9 处 |
| 安全 | **C** | 密码学原语选择正确（Argon2id/AES-GCM/X25519/ChaCha20Poly1305/DPAPI），但**会话层 nonce 复用（P0）**、4 处路径穿越、提权参数面、XSS 面同时存在；安全测试覆盖偏薄 |
| 正确性/数据完整性 | **C+** | 关键链路有真算法，但非原子写（编辑器/vault/PDF 水印）、blob 读回缺失、重启后监听失效、失败仍推进游标等会**静默丢数据** |
| 文档 | **C+** | 文档量足、决策记录诚实；但 DESIGN 正文与 §11 修订并存两套口径、impl/03 与 impl/04 接口段失真、panels 细案"状态未开工"与已交付冲突 |

---

## 5. 复核矩阵（P0/P1 逐条验证记录）

| ID | 复核方式 | 结论 |
|---|---|---|
| SEC-01 | 读 `kvm-core/session.rs:274,304`、`host-core/wire.rs:180-223`、`sync-core/transport.rs:175-176,205-206` | ✅ 成立（同一 `key` 两个 `FrameCipher`，counter 均自 0） |
| SEC-02 | 读 `commands/winops.rs:35,47`、`sys-core/winops.rs:241-259`、`win-integration/maintenance.rs:26,60-74`、`helper/dispatch.rs:55-98,172-189`、`winops_helper.rs:289-295` | ✅ 成立（外置同 id 覆盖；`args.to_vec()` 逐字透传） |
| SEC-03 | 读 `commands/notes.rs:405-439`、`notes-core/canvas.rs:14-21,33-42` | ✅ 成立（`dir` 原样进 `canvas_path`，无 `norm_rel`） |
| SEC-04 | 读 `notes-core/library.rs:52-75` | ✅ 成立（`norm_rel` 不拒 `Prefix`；`disk()` 直接 `root.join`） |
| SEC-05 | 读 `file-core/ops.rs:2192-2199` | ✅ 成立（`\\Windows\\x` 非 `is_absolute`，`join` 替换根） |
| SEC-06 | 读 `file-core/ops.rs:1589-1596` | ✅ 成立（`rel` 来自远端条目名，未过滤 `..`/`\`） |
| SEC-07 | 读 `NotesPanel.tsx:985-990`、`EditorPanel.tsx:675-676`；grep 全库无 DOMPurify | ✅ 成立（`marked@15` 无 sanitize） |
| SEC-08 | 读 `tauri.conf.json:30-36` | ✅ 成立（`scope: ["$APPDATA/**"]`） |
| COR-01 | 读 `clipboard-core/store.rs:300-309,798-838` | ✅ 成立（blob 文本 `content=""`，`get_payload` 文本臂不读 blob） |
| COR-02 | 读 `automation-core/wasm.rs:148-163` | ✅ 成立（`vec![0u8; size]` 先于 `mem.read`） |
| COR-03 | 读 `vault-core/vault.rs:155-156,193-194`、`crypto.rs:70-88` | ✅ 成立（锁的是移动前地址） |
| COR-04 | 读 `editor-core/session.rs:301-316,332-342` | ✅ 成立（`fs::write` 直写 + 随即删草稿） |
| COR-05 | 读 `host-core/device.rs:79-99,134-146` | ✅ 成立（读得到但解不开 → 覆盖重生） |
| COR-06 | 读 `sync-core/module.rs:1356-1367,1401-1407`、`sys-core/module.rs:145-152`、`desktop-core/module.rs:157-161` | ✅ 成立（`stop` 置 `cancel=true`，`start` 未复位） |
| COR-07 | 读 `sync-core/module.rs:1368-1396` | ✅ 成立（任务句柄未存、未 abort） |
| COR-08 | 读 `sync-core/module.rs:510-548` | ✅ 成立（`Err` 仅 warn，`set_cursor(max_ts)` 照推） |
| COR-09 | 读 `notes-core/library.rs:77-83` 与改动链 | ✅ 成立（`from_utf8_lossy` + `write_text` 回写） |
| PERF-01 | 读 `kvm-core/module.rs:839-862` | ✅ 成立（回调内两次 `lock()`） |
| PERF-02 | 读 `SchemaForm.tsx:118-124,178-191` | ✅ 成立 |
| SEC-09 | 读 `proxy-core/sysproxy.rs:104-120`、`service.rs:303,705-712` | ✅ 成立（比对依赖当前 `mixed_port`） |

---

## 6. 建议的整改顺序（详见 `07-prevention.md`）

- **批次 A（热修，当天）**：`SEC-01` + 回归测试（双向 nonce 不得相同）。
- **批次 B（P1 安全，本周）**：`SEC-02`…`SEC-08`（提权参数面 + 4 处路径穿越 + XSS + asset scope）。
- **批次 C（P1 数据完整性）**：`COR-01`…`COR-09`（blob 读回、非原子写、fail-open 身份、重启语义、游标推进、编码回写）。
- **批次 D（P2）**：按 `01-inventory.md` 顺序；每批必须携带负例测试。
- **批次 E（治理）**：`GOV-01`…`GOV-10` 一次性补齐（rustfmt/cargo-deny/dependabot/lint 表/MSRV/CI 门禁）。

**批次完成判定**：`cargo test --workspace` 全绿 + `cargo clippy --workspace --all-targets -- -D warnings` 零告警 + 前端 `tsc/lint/test` 全绿 + 该批次每个缺陷均带**先失败后通过**的负例测试。