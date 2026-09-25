# 11 B8 待裁决方案集（只出方案，不写代码）

> **地位**：本档是 [09-blueprint-alignment.md](09-blueprint-alignment.md) §11「B8 待裁决」的方案落地批（docs-only）。六题各给"现状事实（file:line）→ 选项 → 建议 → 放行后的验收要点 → 裁决问句"；**任何一题动工前须用户明示放行并按 D-22 规则 4 新登记决策编号五要素**（D-31/D-32 已占用预留，D-33 起可登记；D-30 仅存在于 11-editor.md 的预留语境、全仓 grep `plugin-dialog` 零命中＝未引入未登记，动用前先核）。
> **粒度**：承 09 §2.5 六栏标准，但 B8 的性质是"裁决材料"而非"任务书"——放行哪题，哪题才升格为六栏任务书（新立 12 档或并入对应模块档）。
> **来源链**：09 §11（三题原文）＋ §7.3-(a)(b)（B7 转裁决两项）＋ §6.3-(b)（归属翻正归 B8）＋ 台账 91 共性八条（本地 AI 统评闸）＋ 台账 92 §B9b（Pre-socks 内核资产轨预留）。
> **蓝本缺口如实登记**：蓝本原文（`Downloads/DeepSeek-集合软件开源蓝本+(1).md`）2026-09-25 亲验**已不在该路径**，第八节"办公助手"功能清单仓内无抄本——题 1 的 B/C 路线以 09 §11 一句立项框架为限，**放行 B 路线需用户重供蓝本 §8 原文或口述需求清单**，否则方案只能给到骨架级。

## 0. 六题一览

| # | 议题 | 建议 | 放行成本 |
|---|------|------|---------|
| 1 | 办公助手立项 | 默认不做；若做，先 A 路线（AI 增强落现有面板），B 路线待蓝本原文 | A=M / B=XL / C=违背不出机红线 |
| 2 | i18n 复审 | 取 ① 最小止血（安装器只留中文），全量 i18n 维持不做 | ①=S / ②=XL |
| 3 | Pre-Socks 双核 TUN 协作 | 有条件可放行（两角色专槽形制，不做泛化多核） | XL |
| 4 | KVM 拖拽传文件 | 可放行（复用 kvm_send_file 腿，UI 面为主） | M |
| 5 | SSH 连接池统一（§7.3-a 转裁决） | 取"折中：file 域内连接缓存"或维持现状徽标；反对跨域仲裁者 | 折中=M–L / 统一=XL |
| 6 | S3 兼容端点驱动（§6.3-b 归属） | 维持不做（零新包下自写 SigV4 性价比不足） | 自写=M–L |

---

## 1. 办公助手立项（蓝本 §8 OpenLoaf/AionUi 系）

**立项框架原文**（09 §11-1）：新模块 = editor + notes + AI 后端（本地 Agent 运行时或远程 API 凭据）三选一路线评估；**默认不做**。

**现状事实**：
- AI/LLM 接线全仓为**零**——`src/layout/modules.ts:6-20` 的 14 个 ModuleId 无 ai/office/chat；Rust 侧 `openai|deepseek|llm` 零命中；"sidecar" 一词仅指代理内核下载通道（`crates/proxy-core/src/sidecar.rs:1`）。
- 既有面板底座：editor 11 命令（`src-tauri/src/commands/editor.rs:14-155`，含编码档位/会话持久化/草稿恢复）、notes 22 命令（CRUD+wiki 双链+FTS5 搜索+闪卡+画布）。
- **统评闸已预先立**（台账 91 §3 共性八条原文）：「本地 AI 增强（语义搜索/自动打标）统一裁定：可选 sidecar+默认关+内容不出机，归 B8 一并评估，单面板不得私引」——任何路线不得绕过这三闸；共性五条另立「本地端口统一门禁：默认关 + 仅 127.0.0.1 + 无凭据 + 端口自检」。

**三路线评估**：

- **路线 A（不立模块，AI 作"增强"落现有面板）**：语义搜索（notes FTS5 结果向量重排/剪贴板语义检索）、自动打标（notes/clipboard 分类建议）、选区润色/摘要（editor/notes 右键动作）。后端＝本地模型 sidecar（经 B9 artifact 轨下载，默认关）。改动面：新 crate（如 ai-core）+ 各面板挂点 + 一组合规红线批（内容不出机正反例、模型文件走 artifact 校验、127.0.0.1 门禁）。**成本 M**；与"办公助手"蓝本叙事的关系＝只覆盖"AI 增强"一角，不含 Agent 工作流。
- **路线 B（立项新模块"办公助手"，OpenLoaf/AionUi 形制）**：新 ModuleId + SubNAV + editor/notes 融合工作台 + 本地 Agent 运行时（工具调用循环，可操作笔记/文件）。**成本 XL、新模块=架构级承诺**，且**蓝本 §8 功能清单已不可得**（见页首缺口登记）——无原文则连验收点都写不实，本方案拒绝在骨架上装水泥。**前置条件：用户重供蓝本 §8 或口述需求清单。**
- **路线 C（远程 API 凭据）**：违背"内容不出机"统评闸，若采纳属**显式偏离既有裁决**——需单独五要素登记出机面（哪些字节发给谁）、凭据管理（vault 信封复用）、知情门禁（每次出机可见）、以及 DECISIONS §5 相关行翻正。**不建议**。

**放行后验收要点（A 为例）**：① 总开关默认关，关＝模型进程不存在+面板无 AI 入口（ExpertGate 形制）；② 内容不出机红线＝全链路单测钉住网络目标恒 127.0.0.1（镜像 B6 明文四闸的测试形制）；③ 模型资产经 artifact-core 校验（无该 crate 则并入 B9 依赖序，禁私下载通道）；④ aux 窗口负例照 D-28 四面。

**裁决问句**：默认不做是否维持？若做：A（增强落地）/B（立模块，需重供蓝本 §8）/C（出机，需显式翻案）？

---

## 2. i18n 复审

**现状事实**：
- 前端**无任何 i18n 机制**：无 i18next/vue-i18n 依赖；文案硬编码中文（例 `src/layout/modules.ts:32-45` 模块名、`src/modules/kvm/KvmPanel.tsx:523`）。
- 割裂点本体：`src-tauri/tauri.conf.json:44-52` nsis `languages: ["SimpChinese","English"]` + `displayLanguageSelector: false`，wix `language: ["zh-CN","en-US"]`——**安装器提供 English 选项而 UI 恒中文**（D-28 决策 2 当年为"wix 与 NSIS 对齐"引入了双语言，割裂即源于此）。
- 既有裁决（DECISIONS.md:353 §5 表）：i18n 不立项；唯一落点=MODULES[].subtitle 常量化（已兑现于 B0）。

**三选项**：
1. **最小止血**：nsis/wix 语言表收回只留中文（配置一行改动，零新代码），割裂消除；将来真做 i18n 再放回。
2. **全量 i18n**：引入语言包机制＋14 面板×全部硬编码文案迁移＋中英双语资源维护＋设置中心语言开关——**XL 且无需求方**（出货范围＝简体中文个人工具，DECISIONS §5 裁决依据未变）。
3. **维持现状**：保留割裂感（English 装机可选但 UI 中文）。

**建议**：①。这是配置层一分钟决定、产品层零负债；② 维持不做。
**裁决问句**：安装器语言是否收回只留中文（①），还是接受割裂维持现状（③）？

---

## 3. Pre-Socks 双核 TUN 协作（v2rayN 功能，蓝本 §3.2 TUN 段）

**归属链**：二期登记（09 §5.3-7）→ T-B2-12 明示不做 → 本档子方案（09 §11-3）；B9 的 10 档 §B9b 已预留「Pre-socks 内核资产轨」收编位。

**现状事实**：
- **单实例是结构事实**：`crates/proxy-core/src/service.rs:222-233` `Inner { handle: Option<KernelHandle>, kernel: String, .. }` 一个句柄一个内核名；`StatusDto.kernel_running/kernel_id`（:146-147）同为单数——双核不是"没做"，是**数据模型装不下**。
- 三内核注册完备（`sidecar.rs:71-93` KERNEL_ASSETS）；IR 单一混合入站 `IrInbound::Mixed { listen, port }`（`ir.rs:37-40`）；xray 方言渲染为 http(P)+socks(P+1) 双入站（`xray.rs:45-53`）——**"本地再暴露一个 SOCKS 口"在渲染层已有先例**。
- TUN 门禁：`service.rs:762` `enter_tun` 先查 `caps.tun`，xray `caps.tun=false` 如实拒（`xray.rs:16-17`）。

**语义界定**（v2rayN 的 Pre-Socks）＝两内核串联：核 A 专司上游/预代理（本地暴露 SOCKS），核 B 持 TUN 总出口并把 A 当地层 outbound 导入。收益＝"全局 TUN + 特定流量再套一层"的两级代理。

**子方案草图（放行后升六栏）**：
- **两角色专槽，拒绝泛化多核**：`Inner` 扩为 `{ main: Option<..>, presocks: Option<..> }` 两固定角色槽（数组化多核＝状态机×N，明确不做）；`kernel` 字段随槽拆为 per-role。
- IR 加 `pre_socks: Option<..>`（A 核的监听端口/凭据引用）与 B 核 IR 的 `socks` outbound 注入位；三方言渲染各自消费或**如实报不支持**（sing-box 有 SOCKS outbound 先例，xray/mihomo 核账后定 supported 位——禁静默丢参，承 T-B2-7 双保险纪律）。
- 生命周期编排：起序 A→B、停序 B→A，半途失败回滚已起核；两核健康位独立上报（StatusDto 加 `presocks_running/id`，serde default 旧前端可读）。
- 端口门禁复用 term forward 的 bindGate 形制（默认 127.0.0.1、禁 0.0.0.0、占用显式错不自动换口）；TUN 仍在 B 核单点。
- 更新治理归 10 档 B9b（Pre-socks 内核资产注册进 artifact-core，禁第二更新器）。
- 测试面：双核进程编排（桩核可测启停序/回滚）+ 方言渲染矩阵 ×2 + 端口冲突/单核失败 egress 显式降级。

**成本 XL**；放行建议：若代理多出口是真实使用场景则值，否则维持二期登记。**裁决问句**：是否从"二期登记"转正开工？

---

## 4. KVM 拖拽传文件（09 §11-3 / §7.3-(b)）

**现状事实**：
- 推送链已在：设备行「推送文件」钮（`src/modules/kvm/KvmPanel.tsx:537-552`，需出站会话）→ 对话框**手输路径**（:659-673）→ `kvm_send_file(device_id, path)`。
- 全 src **无任何 drop/dragover 处理**；唯一相关命中是 OCR 面板的 onDrop 负例断言（`src/modules/ocr/__tests__/ocrPanel.test.tsx:425`，禁的是 OCR 面板，不涉 kvm）。

**子方案草图**：
- 路径获取是技术要点：HTML5 `dataTransfer` 拿不到宿主文件绝对路径 → 用 **Tauri 原生拖放事件**（窗口级 `onDragDropEvent`；Windows 默认 `dragDropEnabled: true`，**放行后第一枚核证点**＝实启验证 wry 拖放在当前配置下可达）。
- 交互＝设备行/预览区接 drop → 「干跑预览→清单确认→执行」通用组件（台账 91 共性三，`DryRunDialog.tsx` 已由 T-B7-15 立，直接复用）→ 既有 `kvm_send_file` 腿，**零新命令、零字节腿重复建设**。
- 红线批：多文件逐个确认或清单批量（按 D-29 承重"清单确认"口径）；文件名过 `namefix` 类字符闸不适用（接收端是 guest OS 不属远端协议域）——改为**目标 guest 路径合法性校验 + 大小上限配置键**；未连接会话时 drop 显式拒点名原因（禁静默）；拖入非文件（文本/URL）拒。
- 明示边界：guest 内拖回宿主（反向）不做——键鼠投影协议无此回程通道。

**成本 M**；风险面主要是 WebView 拖放与现有键鼠钩子的干扰（放行后冒烟项）。**裁决问句**：放行否？（蓝本原文 v1.1 即标"子方案"，本档已补写）

---

## 5. SSH 连接池统一（B7 转裁决，09 §7.3-(a)＋term 面板徽标）

**现状事实**：
- term＝长会话模型：`crates/term-core/src/ssh.rs:513` `open_shell` 建连+PTY，Handle 锁常驻（:523），注册 `TermSessions`（:652；`session.rs:141` HashMap 会话表），用户断开才关。
- file＝per-op 短连接：`crates/file-core/src/remote/ssh.rs:162-163` 注释自陈「每调用一次独立建连（v1 无连接池…成本登记于 09 §6.2 本行）」。
- 两域**唯一该共享的信任面已单源化**（host-core `ssh_trust`，T-B7-1）；剩下的分歧是"会话生命周期"，不是"事实源"。

**三选项**：
1. **维持现状**：每操作一次握手在网络好的局域网无感；已知债继续挂徽标（零成本，现状登记）。
2. **折中：file 域内连接缓存**（不动 term）：`SshBackend` 前加进程内 (host,port,user)→共享 Handle 的池，空闲超时回收 + 指纹复核（复用 ssh_trust 裁决）+ 断连回落逐次建连；收益＝批量传输/连续浏览免重复握手；**成本 M–L**，改动收在 file-core 一层，不跨域。
3. **彻底统一（仲裁者重构）**：两域生命周期模型根本不同（§7.3-(a) 承重①），统一＝重写 term 会话表 + file 驱动装配 + 双向故障语义——**XL 且无对应收益叙事，反对**。

**裁决问句**：取 2（file 域内池化）？还是维持 1？

---

## 6. S3 兼容端点驱动（§6.3-(b) 归属翻正入 B8）

**现状事实**：file 域驱动注册表已有 sftp/ftp/https/webdav 四协议（B6 批，`register_as` 注册口）；S3 缺席的卡点＝**SigV4 签名**——零新包纪律下自写（HMAC-SHA256 链式派生＋canonical request 全套＋凭据管理）性价比不足（09 §6.3-(b) 翻正原文：「零新包纪律下自写 SigV4 性价比裁决归 B8 待用户」）；徽标已在位（NetdiskSection）。

**三选项**：
1. **维持不做**（默认）：四协议＋WebDAV 已覆盖"自建网盘"主流；徽标继续钉。
2. **自写 SigV4 子集**（仅 path-style + 匿名/List/Get/Put 子命令）：成本 M–L，红线批＝凭据不落盘（复用 AuthSecret 只进不出形制）、禁 query 签名、bucket 名/DNS 校验、官方 checksums 头校验下载；测试可全离线（签名对拍官方测试向量）。
3. **引入 `aws-sigv4`（或等价）依赖**：省签名自写，但破"B6 全批零新包"先例——依赖许可账＋Cargo.lock 变更需显式裁决，且该 crate 拖 aws-types 依赖树，**不建议**。

**裁决问句**：是否真有 S3 端使用场景？无则维持不做（1）。

---

## 7. 裁决汇总（用户一行一题即可）

1. 办公助手：**不做（默认）/ A 增强 / B 立模块（需重供蓝本 §8）/ C 出机（需显式翻案）**
2. i18n：安装器**收回只留中文 / 维持现状**（全量 i18n 默认仍不做）
3. Pre-Socks 双核：**转正开工 / 维持二期登记**
4. KVM 拖拽传文件：**放行 / 维持不做**
5. SSH 连接池：**维持现状 / file 域内池化（折中）**
6. S3 驱动：**维持不做 / 自写子集**

放行项逐条按 D-22 规则 4 新登记决策编号五要素后，升六栏任务书动工；未放行项本档即其"已给一页方案"的兑现记录，09 §11/§12 状态随本批翻正。

**裁决结果（2026-09-25 用户裁决）**：1 办公助手＝**待重供蓝本 §8 原文后再议**；2 i18n＝**维持现状**（不收敛安装器语言，割裂继续登记）；3 Pre-Socks＝**不做**（维持二期登记）；4 KVM 拖拽＝**放行**（→ D-33 / T-B8-1，§8）；5 SSH 连接池＝**维持现状徽标**；6 S3＝**维持不做**。另 D-31/D-32 用户裁决**暂缓**。

---

## 8. T-B8-1 六栏任务书：KVM 拖拽传文件（D-33 放行）

| 栏 | 内容 |
|----|------|
| ①锚点 | `src/modules/kvm/KvmPanel.tsx`：发送腿 `kvmSendFile`(:341)、`clientSessionIds`(:355-357)、边缘说明段 `<DeferredBadge label="拖拽传文件" decisionRef="09 §7.3-(b)" />`（摘除对象）；`src/layout/TitleBar.tsx:55` window API import 形制范本；后端 `crates/kvm-core/src/transfer.rs:164-198`（KVM_TRANSFER_007/008 显式错误面）、`module.rs:274`（"设备 … 无活跃会话"总闸）——**本行零 Rust 改动，只消费**。新文件：`src/modules/kvm/dragDropFlow.ts`、`src/modules/kvm/DragDropSendDialog.tsx`。 |
| ②签名 | `export interface KvmClientDevice { deviceId: string; deviceName: string }`；`export type DropPlan = { kind: "refuse"; reason: string } | { kind: "ready"; files: string[]; devices: KvmClientDevice[] }`；`export function planDropSend(paths: readonly string[], devices: readonly KvmClientDevice[]): DropPlan`（空 paths→refuse"未拖入任何文件"；无设备→refuse 点名"需先建立出站会话"；files 逐字保序全携带）；`export interface SendOutcome { path: string; error?: string }`；`export function aggregateSends(outcomes: readonly SendOutcome[]): { ok: number; failed: number; lines: string[] }`（"成功 X · 失败 Y"，逐条错误行含路径）；对话框 props：`{ open: boolean; files: readonly string[]; devices: readonly KvmClientDevice[]; onSend: (deviceId: string, paths: readonly string[]) => Promise<SendOutcome[]>; onClose: () => void }`，**按次挂载**（`open &&` 条件渲染，B6 ConnectDialog 定格教训）。接线：KvmPanel `useEffect` 内 `inTauri && import("@tauri-apps/api/window").then(...)` 订阅 `onDragDropEvent`，事件 `type==="drop"` 时 `planDropSend(paths, clientDevices)`——refuse→`setError(reason)` 不开对话框；ready→置 dropFiles+开对话框；unmount 退订。 |
| ③数据变更 | 零：无新命令、无新配置键、无持久化、旧数据无关。 |
| ④门禁联动 | D-28 零触碰（`kvm_send_file` 既有 ACL 不动）；aux 窗不渲染 KVM 面板＝无第二消费面；订阅仅面板挂载期（他视图零打扰是本行负例）。 |
| ⑤回归字面测名 | `dragDropFlow.test.tsx`：`kvmDragDrop_emptyPaths_refusesBeforeDialog`、`kvmDragDrop_noClientSessions_namedRefuse`、`kvmDragDrop_readyVerbatimFileOrder`、`kvmDragDrop_aggregate_countsPerLetterAndLines`；`dragDropSend.test.tsx`：`kvmDragDropDialog_sendsSelectedDeviceOncePerFile`（onSend 恰一次、设备 id 逐字）、`kvmDragDropDialog_failureLinesSurfaceAndRecitePaths`、`kvmDragDropDialog_cancelSendsNothing`（onSend 调用数恒 0）；`kvmPanelDragWiring.test.tsx`：`kvmDragDrop_subscribesOnMount_unsubscribesOnUnmount`（本地 `vi.mock("@tauri-apps/api/window")` 捕获 handler + unlisten 计数）；deferred 翻正：`kvmDeferredItems_badgeRetiredWithDelivery`（徽标数恰 0＋`dragDrop|fileDrop` 反转为必在场锚＋"拖拽"字样剥说明段后零实现位＋画饼话术负例保留）。 |
| ⑥完成判据 | 上述 vitest 全绿；`security_config`/workspace 恒绿（零 Rust 变更跑即是负证）；`grep -c "DeferredBadge" KvmPanel 边缘说明段`＝0 且 §7.3-(b) 归属列翻正"已交付（D-33/T-B8-1）"；**实启冒烟两则属人工**（真 drop 达+对端收妥；无会话 drop 点名拒）——WebView2 `dragDropEnabled` 不可达＝本行回挂徽标判红不装完成，登记于 D-33 验收行。 |

实施切法：单提交（纯前端 + deferred 翻正 + 台账/DECISIONS 状态随批）；七门禁全绿不遮罩。
