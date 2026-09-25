# 06 · 文档错误与文档-代码不一致

> 覆盖 `DOC-01`…`DOC-14`。每条含：文档位置 → 真实情况（代码/配置证据）→ 影响 → 修正建议（应改成什么）→ 修复前后对比 → 预防措施。
> 核对原则：**每条都实际打开了对应的代码/配置文件验证**，不做"看起来不对"的推断。
> 先记录已核对一致的部分（避免误伤）：`permissions/*.toml` 17 文件共 255 条 `[[permission]]` 与 `capabilities/main.json` 255 条 `allow-` 一致；`LICENSE` 为 GPL-3.0 全文、`THIRD_PARTY_LICENSES.md` 声明 `GPL-3.0-only` 与 README 一致；`crates/win-integration/tests/conpty_acceptance.rs`、`crates/clipboard-core/benches/clip_store.rs`、`crates/clipboard-core/tests/perf_thresholds.rs`（阈值 50/100ms 与 DESIGN §9.1 逐字一致）均真实存在。

---

# DOC-01 · DESIGN §6「IPC 契约冻结点」的签名与命令名与代码不符　`P2` `[实测]`

**文档位置**：[docs/DESIGN.md:199](../DESIGN.md#L199)（`fn init(&self, ctx: &ModuleContext)`）、[:202](../DESIGN.md#L202)（`fn config_schema(&self) -> ModuleConfig`）、[:247](../DESIGN.md#L247)（`pub async fn clipboard_stack_pop()`）、[:254](../DESIGN.md#L254)（`CapturePort::capture_screen(mode: CaptureMode)`）、[:255](../DESIGN.md#L255)（`OcrPort::recognize(...) -> Result<OcrResult>`）

**真实情况**

| 文档写法 | 代码实际 | 证据 |
|---|---|---|
| `fn init(&self, ctx: &ModuleContext)` | `fn init(&self, ctx: Arc<ModuleContext>)` | [host-core/src/module.rs:116](../../crates/host-core/src/module.rs#L116) |
| `fn config_schema(&self) -> ModuleConfig` | `fn config_schema(&self) -> serde_json::Value` | [host-core/src/module.rs:120](../../crates/host-core/src/module.rs#L120) |
| `clipboard_stack_pop` | 不存在；实为 `clipboard_stack_paste_next` | [commands/clipboard.rs:364](../../src-tauri/src/commands/clipboard.rs#L364) |
| `CapturePort::capture_screen(mode: CaptureMode)` | `fn capture(&self, target: CaptureTarget)` | [host-core/src/ports.rs:196](../../crates/host-core/src/ports.rs#L196) |
| `OcrPort::recognize(...) -> Result<OcrResult>` | 另有 `available_languages(...)`，返回 `Result<Vec<OcrLine>, AppError>` | [host-core/src/ports.rs:205-207](../../crates/host-core/src/ports.rs#L205-L207) |

**影响**：§6 被声明为"IPC 契约冻结点"（`UI-PLAN §8` 要求契约变更走 §6 修订）。开发者照抄签名即编译失败；前端照抄 `clipboard_stack_pop` 即 invoke 报"命令未找到"。

**修正建议**
1. 按真实签名改写 §6.1/§6.4：`Arc<ModuleContext>`、`-> serde_json::Value`；`CapturePort` 改 `fn capture(&self, target: CaptureTarget) -> Result<Frame, AppError>`；`OcrPort` 补 `available_languages` 并改返回 `Vec<OcrLine>`。
2. §6.3 中 `clipboard_stack_pop` 全部替换为 `clipboard_stack_paste_next`（并核对其他示例命令是否仍在册——建议加一段"示例命令必须存在于 `commands/` 的校验说明"）。
3. 在 §6 开头加一句"本节签名由 `cargo doc` 生成的 `host-core` 文档为准；示例仅示意"。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 签名 | 5 处与代码不符 | 与代码一致 |
| 命令名 | 1 个不存在 | 真实命令名 |
| 可执行性 | 照抄即失败 | 可直接使用 |

**预防措施**：§6 这类"契约冻结"章节应**由代码生成或与代码做自动比对**——最小实现是在 `GOV-09` 的契约测试里顺带断言"DESIGN §6 提到的命令名集合 ⊆ 注册表"（把文档纳入同一个集合差集检查）。

---

# DOC-02 · impl/03 截图规格与 D-07/D-08/D-23 裁决及实际命令全面冲突　`P2` `[实测]`

**文档位置**：[docs/impl/03-screenshot-core.md:3](../impl/03-screenshot-core.md#L3)（"Windows.Graphics.Capture（主）"）、[:49](../impl/03-screenshot-core.md#L49)、[:59-64](../impl/03-screenshot-core.md#L59-L64)（"每显示器一个覆盖窗口"）、[:139-142](../impl/03-screenshot-core.md#L139-L142)（`screenshot_cancel()`／`screenshot_apply_tools`／`screenshot_record_start|stop`／`screenshot_pin(output)`）

**真实情况**
- DESIGN §11:383-384 已改判：v1 主路径为 **GDI BitBlt + PrintWindow**，录屏移 v1.1（D-07/D-08）。
- D-23 将"每显示器独立覆盖窗口"记 v1.1。
- `crates/win-integration/Cargo.toml:24-83` 的 features **无任何 `Graphics_Capture`**（PrintWindow 走 `Win32_Storage_Xps`）。
- `src-tauri/src/commands/screenshot.rs` 实际命令为 `screenshot_start / -confirm / -discard / -finish / -scroll_begin|append|finish|discard / -history_list / -pins / -pin_get / -pin_update / -pin_close`——**无** `screenshot_cancel`、`-apply_tools`、`-record_start|stop`、`-pin`。

**影响**：impl/03 自称"代码级细化"，其 P2/P3/P8 三节的现状描述与命令名均失真 → 误导实现、联调与后续里程碑估算。

**修正建议**
1. `:3/:49` 改为"GDI BitBlt（主）+ PrintWindow（窗口）；`Windows.Graphics.Capture` 属 v1.1（录屏，D-07/D-08）"。
2. `:59-64` 加 v1.1 标注（引 D-23）。
3. `:139-142` 按 `commands/screenshot.rs` 实名重写，删除录屏命令；并把"录制控制条"相关行统一标注 v1.1。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 捕获路径 | 声称 Graphics.Capture | GDI/PrintWindow + v1.1 标注 |
| 覆盖窗口 | 声称每显示器一窗 | 标注 v1.1（D-23） |
| 命令名 | 4 个不存在 | 与 `commands/screenshot.rs` 一致 |
| 对应裁决 | 与 D-07/08/23 冲突 | 与裁决一致 |

**预防措施**：`docs/impl/*` 作为"实现细化"必须带**裁决引用与状态标记**（`已实现/已改判/记 v1.1`）；建议在每个 impl 文档头部加一行"最后核对日期 + 对应 commit"，并在审查流程中把"impl 文档与代码一致性"列为固定检查项（本次审查即为一次全量核对）。

---

# DOC-03 · 模块数量与阶段口径三处互斥（13 vs 14）　`P2` `[实测]`

**文档位置**：[README.md:3](../README.md#L3)（"宿主框架 + 13 个功能模块"）、[README.md:8](../README.md#L8)（"P3：自动化引擎（WASM 插件）· 跨设备同步"）、[DESIGN.md:11](../DESIGN.md#L11)（"共 13 个模块"）、[DESIGN.md:41-42](../DESIGN.md#L41-L42)/[:95-111](../DESIGN.md#L95-L111)（模块层与 §3 表均无 `sync`，`automation` 标 P2）

**真实情况**：[src/layout/modules.ts:6-20](../../src/layout/modules.ts#L6-L20) 的 `ModuleId` 联合类型与 [:31-46](../../src/layout/modules.ts#L31-L46) 的 `MODULES` 均为 **14 项**，且 `:44-45` 的 `automation`/`sync` 的 `phase` 都是 **"P2"**；[src/layout/\_\_tests\_\_/modules.test.ts:23](../../src/layout/__tests__/modules.test.ts#L23) 断言 `toHaveLength(14)` 并注明"增删须同步 DESIGN §3"。

**影响**：README/DESIGN 的模块清单与代码/测试不一致；按 DESIGN §3 增删模块会直接踩红 vitest；读者无法判断 `sync` 的阶段归属（DESIGN §9.1 又把它放在"阶段四"，而 README 写 P3）。

**修正建议**
1. `README.md:3` 与 `DESIGN.md:11`/§3 表统一为 **14 个模块**，并补 `sync` 行。
2. README 的 P0–P3 分组改为与 `modules.ts` 的 `phase` 字段 + `DESIGN §9.1` 四阶段一致；`automation`/`sync` 的归属二选一并统一（建议以 `modules.ts` 为单一事实源，DESIGN §3 引用之）。
3. 在 DESIGN §3 加注："模块清单的权威定义在 `src/layout/modules.ts` 的 `MODULES`，本表须与之同步（有 pin 测试）"。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 模块数 | 文档 13 / 代码 14 | 统一 14 |
| `sync` 行 | 缺失 | 已列 |
| 阶段口径 | README P3 / DESIGN 阶段四 / 代码 P2 | 统一（并注明权威源） |
| 与测试关系 | 按文档改会踩红测试 | 文档与测试互证 |

**预防措施**：把"文档中的枚举型事实（模块数、命令数、主题数、权限数）"统一改为**引用权威源 + 数量自动核对**；本次审查中用脚本核对 255 条命令/权限一致即是可复用的做法。

---

# DOC-04 · panels 细案"状态：未开工"与已交付代码冲突　`P2` `[实测]`

**文档位置**：[docs/panels/2026-09-19/09-screenshot.md:3](../panels/2026-09-19/09-screenshot.md#L3)、[10-ocr.md:3](../panels/2026-09-19/10-ocr.md#L3)、[15-host-shell.md:3](../panels/2026-09-19/15-host-shell.md#L3)（均"状态：未开工"）；`09-screenshot.md:7` 与 `15-host-shell.md:7-8` 称"`src/modules/` 下没有 screenshot/ocr 目录""MainWorkbench 落『模块界面待实现』兜底三元（:234-266）""`moduleId="clipboard"`（:201）"

**真实情况**
- [src/modules/screenshot/ScreenshotPanel.tsx](../../src/modules/screenshot/ScreenshotPanel.tsx)、[src/modules/ocr/OcrPanel.tsx](../../src/modules/ocr/OcrPanel.tsx) **均已存在**。
- [src/layout/panels.tsx:61-117](../../src/layout/panels.tsx#L61-L117) 为 `PANELS` 注册表（穷尽 `Record<ModuleId,…>`）。
- [src/windows/MainWorkbench.tsx:190-191,231](../../src/windows/MainWorkbench.tsx#L190-L191) 走 `PANELS[moduleId]`，且 `SchemaForm moduleId={settingsModule}`（不再是硬编码 clipboard）。
- `DECISIONS D-29` 实施记录（[DECISIONS.md:337](../DECISIONS.md#L337)）与 `09 §2` 均记 B0 已完成。

**影响**：细案开头即给出已被推翻的现状；读者据其"重做兜底三元/修 moduleId"会改错方向（浪费工时、可能回退已完成修复）。

**修正建议**：三档 `状态：` 改为"B0 已完成（2026-09-19，`dab1074…96894da`）；B4/B7 归 09 §2 批次表"，并删除已修复的"现状问题"条（或就地标注"已修（见 `PANELS` 注册表）"）。同类需要处理的还有 `00-ui-layout-spec.md` 等引用旧布局的条目（建议一次性核对 panels 全目录的"状态"字段）。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 状态字段 | 未开工 | 已完成 + 批次归属 |
| 现状问题 | 已修复项仍列为待做 | 标注已修/删除 |
| 读者动作 | 可能回退已修代码 | 直接进入后续批次 |

**预防措施**：为 `docs/panels/*` 建立**状态字段的单一来源**（如统一表格 `状态 / 批次 / commit / 核对日期`），并在每批交付时更新；可把"状态字段与实际目录存在性"做成轻量脚本核对（存在性检查成本极低）。

---

# DOC-05 · README 技术栈版本过时（React 19 vs 18）　`P3` `[实测]`

**文档位置**：[README.md:13](../README.md#L13)（"前端：React 19 + …"）

**真实情况**：`package.json:23-24` 为 `react`/`react-dom` `^18.3.1`，`package.json:30-31` 为 `@types/react ^18.3.12`；`DESIGN.md §2.1:31` 与 `§7:272` 均写 **React 18**。

**影响**：README 与 DESIGN/依赖清单互相矛盾；新成员按 README 预期 React 19 的特性（如 `use`、Actions）会踩版本事实。

**修正建议**：`README.md:13` 改为"React 18 + TypeScript 5 + Fluent UI React 9 + Vite 5"（与 `package.json` 及 DESIGN §2.1 对齐）。

**修复前后对比**：修复前 README 说 19、实际 18；修复后三处一致。

**预防措施**：README 的技术栈段改为"指向 `package.json`/`Cargo.toml` 的摘要"，或在 CI 加"README 声明的版本与 lockfile 主版本一致"的轻量断言（对 React/Rust 两条主线即可）。

---

# DOC-06 · IMPLEMENTATION 的决策编号区间过期　`P3` `[实测]`

**文档位置**：[docs/IMPLEMENTATION.md:20](../IMPLEMENTATION.md#L20)（"DECISIONS.md 决策记录（D-01…D-23）"）

**真实情况**：`docs/DECISIONS.md` 章节已到 **D-33**（[:325](../DECISIONS.md#L325) D-29、[:350](../DECISIONS.md#L350) D-33）；`DESIGN.md §11:394` 亦引用 D-28。

**影响**：按 D-01…D-23 检索会漏掉 D-24…D-33（含 D-24 免密解锁、D-27 性能基准、D-28 发布安全面等关键裁决）。

**修正建议**：改为"（D-01…D-33）"，或更稳妥地去掉硬编码区间，写"decision 列表以 `DECISIONS.md §1` 摘要表为准"。

**修复前后对比**：修复前区间过期（漏 10 条决策）；修复后完整或改为引用式。

**预防措施**：避免在索引类文档里写"范围式"引用（`D-01…D-xx`），统一用"见 `DECISIONS.md §1`"；若必须写范围，纳入交付检查（每次新增决策时同步更新引用）。

---

# DOC-07 · DECISIONS §1 摘要表不完整（缺 D-29/D-33，D-30 无定义）　`P3` `[实测]`

**文档位置**：[docs/DECISIONS.md:13-40](../DECISIONS.md#L13-L40)（§1 裁决摘要表止于 D-28）、[:3-5](../DECISIONS.md#L3-L5)（编号规则"追加型，不复用、不重排"）

**真实情况**：正文含 D-29（[:325](../DECISIONS.md#L325)）与 D-33（[:350](../DECISIONS.md#L350)）两节，但 §1 摘要表未登记；**D-30 全仓无定义**（仅 `docs/impl/11-b8-proposals.md:3` 记为"预留语境"），D-31/D-32 被 D-33 引为"暂缓"但无章节。

**影响**：决策清单与状态表不完整；D-29/D-33 的批次与状态无法从摘要表获得（读者需翻正文），决策号的连续性也被打断（违反自身"不复用、不重排"的编号规则说明）。

**修正建议**
1. §1 表补 D-29、D-33 两行（主题/类型/优先级/批次/状态）。
2. 对 D-30 明确标注"**保留未用（无定义）**"；为 D-31/D-32 补"预留·暂缓（引用 D-33 的说明）"行。
3. 在 §1 表头加一句"本表须与正文一一对应；新增决策必须同时更新本表"。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 摘要表 | 止于 D-28 | 覆盖 D-01…D-33 |
| 空洞号 | D-30 无定义、D-31/32 无章节 | 显式标注状态 |
| 编号规则 | 被破坏且无说明 | 自洽 |

**预防措施**：把"决策号 → 摘要表"做成**同 PR 强制更新**（可在模板 PR 描述里加勾选项）；并考虑为 `DECISIONS.md` 写一个轻量校验脚本（编号连续、每个号在摘要表出现、章节存在）。

---

# DOC-08 · UI-DEMO 与代码注释引用不存在的 DESIGN 小节　`P3` `[实测]`

**文档位置**：[docs/UI-DEMO.md:21](../UI-DEMO.md#L21)（"DESIGN §3.2 全局工具栏"）、[:29](../UI-DEMO.md#L29)（"DESIGN §3.5 状态栏"）、[:31](../UI-DEMO.md#L31)（"DESIGN §10 通知系统"）、[:69](../UI-DEMO.md#L69)（"DESIGN §3/§7"）；代码注释：[src/windows/MainWorkbench.tsx:35](../../src/windows/MainWorkbench.tsx#L35)（"DESIGN §3 像素级布局"）、[src/layout/SubNav.tsx:4](../../src/layout/SubNav.tsx#L4)（"DESIGN §3.4"）

**真实情况**：`docs/DESIGN.md` 的目录为 §1…§11——**§3 = 模块划分与优先级（表格）**、**§10 = 蓝本优化决策记录**；全文无 §3.2/§3.4/§3.5，也无"通知系统"章节。

**影响**：核心 UI 文档与代码注释共同指向不存在的小节 → 追溯链断裂（读者无法定位依据）。

**修正建议**（二选一）
1. **推荐**：在 DESIGN 新增"§12 UI 布局 / 导航 / 通知"章节承载这些内容，然后把 UI-DEMO 与两处代码注释的引用改为 `§12.x`；
2. 或改写引用为"见 UI-DEMO §2 对应行 / UI-PLAN §3"，并同步修改两处代码注释。

**修复前后对比**：修复前 4 处文档 + 2 处注释悬空引用；修复后引用可解析。

**预防措施**：加"文档锚点检查"（markdown 链接与小节号解析）到审查脚本；代码注释引用文档时，注明"章节标题"而非仅编号（标题更稳定、更易检索）。

---

# DOC-09 · UI-PLAN 版本基线、目录结构与任务项与现状冲突　`P3` `[实测]`

**文档位置**：[docs/UI-PLAN.md:12](../UI-PLAN.md#L12)（"Zustand 4"）、[:36-64](../UI-PLAN.md#L36-L64)（§3 目录树：`windows/ScreenshotOverlay.tsx`、`windows/OcrResultOverlay.tsx`、`modules/placeholder/GenericModule.tsx`、`modules/clipboard/{EntryCard,GroupFilter,SecretBadge,store,ipc}`、`settings/SchemaForm.tsx` + `stores/session.ts|notifications.ts`）、[:108](../UI-PLAN.md#L108)（U4-5 "录制控制条…依赖 P7"）

**真实情况**
- `package.json:25` 为 `zustand ^5.0.15`。
- 实际 `src/windows/` 是 `OverlayShot.tsx`/`PinWindow.tsx`/`QuickPanel.tsx`（**无** `ScreenshotOverlay`/`OcrResultOverlay`）；无 `modules/placeholder/`；`src/modules/clipboard/` 为 `ClipboardPanel.tsx`/`dib.ts`/`DibThumb.tsx`/`display.ts`/`panels/*`（无 `EntryCard`/`store`/`ipc`）；已建 `src/theme/theme.ts` 与 `src/components/*`。
- D-08（DESIGN §11:384）要求"UI 不得暗示录屏可用"，而 U4-5 把录屏 UI 列为交付。

**影响**：计划与已交付形态脱节（新成员按目录树找文件会找不到）；U4-5 直接抵触 D-08 红线（`modules.test.ts` 已 pin"录屏/录制"禁词）。

**修正建议**
1. `:12` 改 **Zustand 5**。
2. §3 目录树按实际重写，或整体标注"目标态（已实现形态见 `src/`）"。
3. U4-5 删除或移入 v1.1 并标注"（D-08：v1 不交付录屏 UI）"。
4. 建议在 UI-PLAN 头部加"本文为设计意图，落地形态以 `src/` 为准"的声明，避免读者误当现状。

**修复前后对比**

| 维度 | 修复前 | 修复后 |
|---|---|---|
| Zustand | 4 | 5 |
| 目录树 | 与实际不符 | 与实际一致或显式标为目标态 |
| U4-5 录屏 UI | 列为交付（违反 D-08） | 移 v1.1 并标注 D-08 |
| 可读性 | 易误认为现状 | 明确"意图 vs 现状" |

**预防措施**：设计文档区分"意图态/现状态"两种描述并显式标注；凡涉及被裁决裁剪的功能（D-08/D-09 等）在计划文档中统一加"红线标注"。

---

# DOC-10 · impl/04 列出不存在的翻译命令　`P3` `[实测]`

**文档位置**：[docs/impl/04-ocr-core.md:117](../impl/04-ocr-core.md#L117)（`#[tauri::command] ocr_translate(text, to) -> Result<String, AppError>`）

**真实情况**：`src-tauri/src/commands/ocr.rs` 实际命令为 `ocr_recognize / ocr_config_get / ocr_engine_status / ocr_copy_text / ocr_export`——**无 `ocr_translate`**（D-09 已将翻译列入 v1.1）。

**影响**：该节未像 O3/O6 那样标注 v1.1，翻译命令被当作在册契约（前端可能据此实现调用）。

**修正建议**：`:117` 移入 v1.1 小节，或就地标注"（v1.1，D-09；v1 无此命令）"。

**修复前后对比**：修复前命令被当作已交付；修复后明确 v1.1。

**预防措施**：同 `DOC-02`——impl 文档中的"命令/接口清单"应与 `commands/` 的注册表做集合核对（可并入 `GOV-09` 的契约脚本）。

---

# DOC-11 · DESIGN 正文与 §11 修订并存两套口径　`P3` `[实测]`

**文档位置**：[DESIGN.md:99](../DESIGN.md#L99)（截图含"Windows.Graphics.Capture 捕获"）、[:152](../DESIGN.md#L152)（"录屏输出走 FFmpeg Sidecar"）、[:100](../DESIGN.md#L100)/[:157](../DESIGN.md#L157)（OCR"双引擎 + 翻译"）、[:186](../DESIGN.md#L186)（blob `blobs/{module}/{yyyy-mm}/{hash}`）

**真实情况**：同一文件的 §11:383-388 已改判为"GDI/PrintWindow 为 v1 主路径""录屏移出 v1""v1 仅 `Windows.Media.Ocr` 单引擎""扁平 `blobs/{module}/{hash}`"（D-06/D-07/D-08/D-09）。

**影响**：单文件内两套口径并存 → 只读正文者会按已废止口径实现（例如按月目录写 blob，或按双引擎设计接口）。

**修正建议**：在 §2.1/§4.2/§4.3/§5.2 逐处加内联标注"（已由 §11 修订，见 D-0x）"，**或**直接以修订后口径改写正文（推荐后者：正文即事实，§11 保留为"修订登记/历史"）。

**修复前后对比**：修复前"正文说 A、§11 说 B"；修复后正文为唯一实施口径，§11 仅记录变更史。

**预防措施**：把"修订登记"与"正文更新"绑定为一条规则：**凡进入 §11 的改判，必须同时就地更新正文**（§11 只保留"何时为何改"，不承载唯一有效信息）；本次发现的其他同类（`DOC-02`/`DOC-10`）同批处理。

---

# DOC-12 · panels/02-proxy 决策编号误引　`P3` `[实测]`

**文档位置**：[docs/panels/2026-09-19/02-proxy.md:39](../panels/2026-09-19/02-proxy.md#L39)（"系统代理兜底恢复 | FlowZ | ✅ D-06"）

**真实情况**：[docs/DECISIONS.md:76](../DECISIONS.md#L76) 的 **D-06 = "blob 路径采用扁平结构"**，与系统代理恢复无关（代理恢复属代理 PR 批 / D-29）。

**影响**：以决策号溯源时指向错误裁决（评审者按 D-06 去核对会得到无关内容）。

**修正建议**：改为"✅（代理 PR 批，见 09 §5 / D-29）"。

**修复前后对比**：修复前误引；修复后指向正确裁决。

**预防措施**：引入决策号时**同时写主题词**（如"D-06（blob 扁平路径）"），便于人工与脚本双向校验；`DOC-07` 的校验脚本可扩展检查"引用号是否存在且主题匹配"。

---

# DOC-13 · DESIGN §9.2 性能回归口径与 CI 实际不符　`P3` `[实测]`

**文档位置**：[DESIGN.md:354](../DESIGN.md#L354)（"性能回归 | CI 每次提交跑基准：启动/内存/搜索响应/模块加载，退化超阈值即失败"）

**真实情况**：[.github/workflows/ci.yml:30-31](../../.github/workflows/ci.yml#L30-L31) 仅 `cargo bench --workspace --no-run`（**只编译不执行**，无 baseline 对比）；阈值断言由 `cargo test`（`ci.yml:27-28`）里的 `crates/clipboard-core/tests/perf_thresholds.rs` 承担；启动/内存无自动化（D-27 决策 3 已记 v1.1）。

**影响**：§9.2 承诺的"退化超阈值即失败"在 CI 中不存在，易被误认为已有回归门禁（评审时"性能已验证"是假信号）。

**修正建议**：§9.2 该行改为"基准矩阵仅编译校验（`ci.yml`）；阈值断言随 `cargo test` 常跑（`perf_thresholds.rs`，p95 50/100ms）；启动/内存记 v1.1（D-27）"，与 `GOV-08` 的处理保持一致。

**修复前后对比**：修复前口径超出实现；修复后与 CI 一致。

**预防措施**：同 `GOV-08`——**文档中的验收标准必须可执行**；建议在文档评审清单里加"该标准的验证方式（命令/测试文件）"一栏。

---

# DOC-14 · DESIGN §2.3 目录结构与仓库实际不符　`P3` `[实测]`

**文档位置**：[DESIGN.md:84-88](../DESIGN.md#L84-L88)（`resources/`、`src/dock/ layout/ quick-panels/ widgets/ overlays/`、`crates/*` 未含 `nexusforge-helper`/`sync-core`）

**真实情况**：根目录**无 `resources/`**；`src/` 实际为 `components/ ipc/ layout/ modules/ monaco/ settings/ stores/ styles/ theme/ windows/overlay/`（无 `dock/`/`quick-panels/`/`widgets/`/`overlays/`）；`crates/` 含 `nexusforge-helper`、`sync-core`。

**影响**：新成员按图索骥找不到目录；与 README:19-31（已较准确）互相矛盾。

**修正建议**：按实际重绘 §2.3——可直接对齐 `README.md:19-31` 的结构描述，并补齐两个缺失 crate。

**修复前后对比**：修复前目录图失真；修复后与实际一致（且与 README 一致）。

**预防措施**：目录结构类描述改为"以仓库实际为准"的简表，并注明"完整清单见 `crates/` 与 `src/`"；可加轻量脚本核对"文档提到的顶层目录/ crate 是否存在"（成本极低）。

---

# 附：文档命令可执行性核对表（实测）

| 命令 | 出处 | 是否可执行 | 说明 |
|---|---|---|---|
| `npm install` | README:39 | ✅ | `package.json` 存在 |
| `npm run tauri dev` | README:42 | ✅ | `scripts.tauri = "tauri"`，`@tauri-apps/cli` 在 devDependencies |
| `npx tsc --noEmit` | README:45、REVIEW:17 | ✅ | `tsconfig` 就绪（本次未执行，见 README §2） |
| `npm run build` | README:46 | ✅ | `tsc && vite build` |
| `npm run lint` | ci.yml:45 | ✅ | `eslint .` |
| `npm run test` | ci.yml:47 | ✅ | `vitest run` |
| `cargo test --workspace` | README:49 等 | ✅ | 本次实测 985 通过 / 0 失败（`_raw/cargo-test.txt`） |
| `cargo test -p win-integration --test conpty_acceptance -- --ignored` | README:52、impl/06:107 | ✅ | 测试文件存在（本次实测该二进制 1 通过 / 1 忽略） |
| `cargo bench --workspace --no-run` | ci.yml:31 | ✅ | `clipboard-core/benches` 存在 |
| `cargo fmt --all --check` | ci.yml:24 | ✅（默认配置） | 无 `rustfmt.toml`，走 rustfmt 默认（`GOV-01`） |
| `npm run tauri -- build` | nightly.yml:26、release.yml:31 | ✅ | 同上 |
| `node tools/gen-third-party-licenses.mjs` | THIRD_PARTY_LICENSES.md:3 | ✅ | 脚本存在；需 `cargo metadata` 成功 + `npm install` |
| `cargo tauri dev` | IMPLEMENTATION.md:44、UI-PLAN:74 | ⚠️ 条件可执行 | 需本机 `cargo install tauri-cli`；仓库仅提供 npm 侧 CLI，未声明该前置 |
| `cargo test -p artifact-core` | impl/10:178 | ❌ | `crates/artifact-core` 未创建（D-32 暂缓） |
| `clipboard_stack_pop` / `ocr_translate` / `screenshot_record_start` 等示例 | DESIGN:247、impl/04:117、impl/03:141 | ❌ | 命令不存在（见 `DOC-01`/`DOC-10`/`DOC-02`） |

**汇总**：文档命令可执行性整体良好（15 项中 11 项可直接执行）；不可执行的 4 项中，3 项属"接口清单未随裁决更新"（`DOC-01/02/10`），1 项属"未开工模块的远期命令"（`impl/10`）——**建议统一在文档中标注 v1.1/未开工**。