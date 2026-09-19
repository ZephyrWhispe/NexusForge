# NexusForge 决策记录（Decision Log）

> 版本：v1.0 ｜ 日期：2026-09-18 ｜ 上游依据：[REVIEW-2026-09-18.md](./REVIEW-2026-09-18.md)（全量审查报告）
> 本文档是 v1.0 规范之外的**追加决策来源**：凡与 [DESIGN.md](./DESIGN.md) 存在偏离或原规范未覆盖的取舍，以本文档裁决为准；涉及规范条文变更的条目已同步登记到 DESIGN.md §11。
> 编号规则：`D-NN`（追加型，不复用、不重排）；状态取值：`已裁决（文档生效）` / `待实施` / `实施中` / `已完成`。

---

## 1. 裁决摘要

| 编号 | 主题 | 类型 | 优先级 | 批次 | 状态 |
|------|------|------|--------|------|------|
| D-01 | 门禁先行：CI 与工具链先于功能补齐 | 流程 | P0 | 2 | 已完成（待首次 CI 绿 run） |
| D-02 | 消除模块间直接依赖（O1 红线） | 补实现 | P0 | 1 | 已完成 |
| D-03 | 事件总线统一背压 API（O8） | 补实现 | P1 | 2 | 已完成 |
| D-04 | 剪贴板敏感数据改用 AES-256-GCM 信封加密 | 补实现 | P0 | 1 | 已完成 |
| D-05 | 剪贴板 blob 生命周期治理（随删随清 + 覆写 + GC） | 补实现 | P0 | 1 | 已完成 |
| D-06 | blob 路径采用扁平结构 | 改规范 | P2 | — | 已裁决（文档生效） |
| D-07 | 截图捕获 v1 以 GDI/PrintWindow 为主路径 | 改规范 | P2 | — | 已裁决（文档生效） |
| D-08 | 录屏（P7）移出 v1，列入 v1.1 | 改规范 | P1 | 3 | 已裁决（文档生效） |
| D-09 | OCR v1 单引擎 + 保留引擎抽象扩展点 | 改规范 + 补骨架 | P1 | 2/3 | 第 1 步已完成（第 2 步待批次 3） |
| D-10 | OCR「复制全部」走剪贴板回写窗口 | 补实现 | P1 | 1 | 已完成 |
| D-11 | 编辑器 PDF 保持纯 Rust（lopdf） | 改规范 | P2 | — | 已裁决（文档生效） |
| D-12 | 同步库归位 `db/sync.db` | 补实现 | P1 | 1 | 已完成 |
| D-13 | WinOps v1 交付范围收敛为 5 命令 | 改规范 | P2 | 3 | 已裁决（文档生效） |
| D-14 | 引入 Zustand 三 store，替换轮询与静态状态 | 补实现 | P1 | 1 | 已完成 |
| D-15 | 锁策略统一（parking_lot + 回调无锁快照） | 补实现 | P0 | 2 | 已完成 |
| D-16 | host-core 收敛公共工具与 ModuleStateCell | 补实现 | P1 | 2 | 已完成 |
| D-17 | 前端工程门禁（ESLint + Vitest + CI） | 补实现 | P0 | 2 | 已完成 |
| D-18 | 前端组件基线 + 破坏性操作统一二次确认 | 补实现 | P1 | 2 | 已完成 |
| D-19 | 全局错误通道与模块重启入口 | 补实现 | P0 | 1 | 已完成 |
| D-20 | ConPTY 诊断探针与验收门禁治理 | 治理 | P0 | 0 | 已完成 |
| D-21 | 仓库治理与开源合规补齐 | 治理 | P0 | 0 | 已完成 |
| D-22 | 批次划分、执行顺序与质量门槛 | 流程 | P0 | — | 已裁决（文档生效） |
| D-23 | 多显示器 / 混合 DPI 策略 | 补实现 | P1 | 1 | 已完成 |

**批次含义**：0 = 止血（无设计风险）；1 = P0 正确性与安全红线；2 = 门禁与一致性重构；3 = 功能补齐与规范落地。详见 [REVIEW-2026-09-18.md](./REVIEW-2026-09-18.md) §7。

---

## 2. 架构与规范类裁决

### D-01 门禁先行：CI 与工具链先于功能补齐

- **背景**：审查实测仓库无 `.github/`、无 clippy 工具链、无前端 lint/测试配置。DESIGN §9.3 与 IMPLEMENTATION §3 的"验收总闸"要求 CI 全绿，但该闸门从未存在，导致"测试全绿"只能是本地手工结论。
- **决策**：**在补齐任何 P1/P2 功能之前**，先建立最小可用门禁：`ci.yml`（rustfmt + clippy + `cargo test` + `tsc` + ESLint）、`release.yml`、`nightly.yml`；并先安装 `clippy` 与前端测试依赖。
- **依据**：本地已出现 44 处静默吞错、186 处锁 `unwrap`、生命周期泄漏等缺陷，均是"无门禁即无回归保护"的直接产物。补功能会放大存量风险。
- **代价**：短期交付节奏变慢一天量级；换来后续所有改动的可验证性。
- **验收**：`gh` 上出现首个绿色 CI run；门禁红灯能真实拦住一次人为缺陷。
- **实施记录（2026-09-18）**：`.github/workflows/{ci,release,nightly}.yml` 落地。ci.yml：windows runner 上 `cargo fmt --all --check` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo test --workspace`，ubuntu runner 上 `tsc --noEmit` + `npm run lint` + `npm run test`。release.yml（tag v* / 手动）与 nightly.yml（cron + artifact）走 `tauri build`，因 updater 私钥与代码签名证书尚未持有，以 `--config` 临时关闭 `createUpdaterArtifacts`（文件内有待办注释）。rustfmt 基线（约千处重排）作为独立提交进入同一批次，避免污染功能性 diff。
- **完成证据（2026-09-18，本地等价门禁全绿）**：`cargo fmt --all --check` OK；`cargo clippy --workspace --all-targets -- -D warnings` 零告警（清理 40+ 条，含两个真缺陷：kvm-core pairing.rs 的 reject 闭包内 future 从未 await → **PairReject 帧从未发出**，改宏内联 await 并以 `pair_wrong_code_rejected` 断言 `KVM_PAIR_005` 回归锁死；clipboard-core secrets.rs `if strong {...} else {...}` 两分支同体——数字规则的 `^…$` 锚点已表达整串语义，删除伪分支）。红灯拦截演示：clippy `err_expect` 在新增回归测试写法上真实报错过一次并已修。GitHub 首绿 run 待推送后以 `gh run list` 复核（D-01 验收尾项）。

### D-02 消除模块间直接依赖（O1 红线）

- **背景**：实测 `notes-core → file-core`（`crates/notes-core/Cargo.toml:9`，使用点 `src/library.rs:11`、`src/module.rs:61`）与 `sync-core → kvm-core`（`crates/sync-core/Cargo.toml:9`，使用点 `src/transport.rs:18-22`）。
- **决策**：**补实现，不降低标准**。`StorageDriver` / `DriverRegistry` 上移到 `host-core`（或独立 `storage-driver` crate）；`sync-core` 所需的会话加密与设备身份改由 `host-core::ports::{CryptoPort, SessionPort}` 暴露，`kvm-core` 提供实现并注册。
- **备选与否决**：否决"修改 DESIGN 允许例外"——O1 是本项目相对蓝本的核心改进（防环形依赖），一旦开口会迅速退化；且两处依赖均可用现成的 port trait 模式消解，无技术障碍。
- **实施修订（2026-09-18）**：`SessionPort` 方案在实施中被否决——运行时 sync↔sync 是唯一对端（kvm 模块从不参与 SYNC 握手），端口化只会把 sync 协议代码搬进 kvm 并引入 async-dyn 体操；改为**机械上移**：信任根（DeviceIdentity/PairStore）与线协议原语（帧编解码/HKDF/FrameCipher）整体上移至 `host-core::{device, wire}`，kvm-core 原路再导出保持 API 不变。存储侧按原案：`StorageDriver`/`FileEntry`/`DriverInfo` 上移 `host-core::storage`，新增 `StoragePort` 由 file-core 实现桥接、宿主注册。
- **验收**：`cargo tree -p notes-core` 与 `-p sync-core` 中不再出现其他模块 crate；DESIGN O1 逐条可验证。
- **完成证据（2026-09-18）**：`host-core::device`（X25519 身份 + DPAPI 落盘 + PairStore，4 测试）与 `host-core::wire`（帧编解码 + HKDF 派生 + ChaCha20-Poly1305 FrameCipher，含重放/篡改负例）落地；kvm-core 删除 `identity.rs`、`session.rs` 线协议段改再导出（26 测试 + loopback 全绿）；sync-core 全面切至 host-core 并移除 kvm-core 依赖（11 测试 + loopback）；`host-core::storage::StoragePort` + file-core `FileStoragePort` 桥 + src-tauri 注册，notes-core 移除 file-core 依赖、init 经端口取驱动；回归测试 `module_init_resolves_driver_via_storage_port`（正例经端口注入 + 负例缺端口明确报错）、`storage_port_bridge_resolves_local_driver`。实测 `cargo tree -p notes-core` / `-p sync-core`（normal+build）均无其他模块 crate；`cargo test --workspace` 全绿。

### D-03 事件总线统一背压 API（O8）

- **背景**：`host-core/src/events.rs:182` 的 `subscribe_debounced` 全仓仅自身单测调用；无 throttle 实现；四个模块各自实现 50ms / 1s / 8ms 合并，阈值互不一致。
- **决策**：`EventBus` 增加 `subscribe_throttled(topic, interval)` 与 `publish_merged(topic, key, window)`；背压策略作为元数据写入 `TOPIC_REGISTRY`（`(topic, desc, BackpressurePolicy)`）；clipboard / sys.metrics / term.output / operation.progress 四个生产端改为调用统一 API。
- **规范对齐**：DESIGN §2.2 O8 要求"剪贴板 300ms 合并、监控指标 1s 节流"，当前 clipboard 实为 50ms，实施时按规范值改为 300ms。
- **验收**：新增集成测试「1s 内发布 1000 条 → 订阅者收到 1 条」；各模块不再存在自建合并计时器。
- **实施记录（2026-09-18）**：`TOPIC_REGISTRY` 升级为 `(topic, desc, BackpressurePolicy)` 三元组，策略枚举 `None / Merged{window_ms} / Throttled{interval_ms} / Batched{window_ms,max_bytes}`，配套 `backpressure_policy()`/`merged_window()` 读取器，阈值全仓唯一来源落定。`EventBus` 新增三 API：**`publish_merged(event, key, window)`**（生产端合并：每主题一条后台 std 线程按窗口节拍冲刷 last-wins 快照，普通线程/sys worker 可直接调用，线程持 `Weak<Bus>` 随总线释放自灭）；**`flush_merged(topic, key)`**（终态立即冲刷，冲刷后同快照不二次投递）；**`subscribe_throttled(topic, interval)`**（消费端前沿+后随节流，本次四个高频主题均为生产端合并、暂无 Throttled 策略用户，API 按裁决落地并全测，为后续高频消费 UI 保留统一入口）。生产端接线：`clipboard.captured` 三处发布改 `publish_merged(key="captured", 300ms)`——按 O8 规范值落地（审查记载的"50ms 合并"实为 DB 入库批调度，事件通知此前**逐条直发、并无合并**；入库批更名 `DB_BATCH_WINDOW` 以绝混淆，并顺带消灭"阈值不一致"的真实形态：不是 50≠300，而是通知根本未节流）；`sys.metrics` 改 `publish_merged(key="sample", 1s)`（PDH 1s 采样是数据生成节拍，保留）；`operation.progress` 改 `publish_merged(key=op_id, 200ms)` 且 Done/Failed 先 `flush_merged` 再发终态事件——file-core `Reporter` 的 `last_emit`/`PROGRESS_INTERVAL` 自建计时器删除（快照逐块上报，限频归总线，单批 to_value 成本 µs 级可忽略）；`term.output` 登记 `Batched{8ms,64KB}`——字节流为累计拼接，last-wins 合并会**丢终端输出**，故读取循环保留 term-core，但 `BATCH_WINDOW_MS`/`BATCH_MAX` 模块常量删除，改 `batched_params()` 从注册表读取（参数唯一来源归一）。**漂移补充**：审查列四家，实际第五家 `kvm-core/transfer.rs` 的 `PROGRESS_INTERVAL` 200ms `last_report` 计时器（`kvm.file_progress`）同属自建合并计时器，按"各模块不再存在自建合并计时器"验收标准一并收编（逐块上报快照 + 总线 200ms 合并，key=transfer_id）。
- **完成证据（2026-09-18）**：`cargo fmt --check` 0、`cargo clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` 全绿。回归测试 9 枚：host-core 6（O8 规范值冻结 300/1000/200/8+64KB；**验收测试** `publish_merged_thousand_burst_delivers_one`——1s 内 1000 条突发订阅端恰收 1 条末值且无心跳补发；双 key 不互并；`flush_merged` 即时性+不二次投递；普通线程合并发布与非注册主题报错；`subscribe_throttled` start_paused 虚拟时钟下前沿立即/窗口末补发最新）；clipboard `burst_captures_merge_into_fewer_events_than_entries`（8 连击入库 8 条不受影响、通知合并至 ≤2）；file-core service `progress_merges_on_bus_and_done_flushes_immediately`（10MB/3 分块复制：进度事件经总线合并且 state=done 终态快照必达）；term-core `batch_params_sourced_from_registry`（批参数唯一来源断言）。

### D-06 blob 路径采用扁平结构（改规范）

- **决策**：**修订 DESIGN §5.2 规则 3**，blob 路径由 `{appData}/blobs/{module}/{yyyy-mm}/{hash}` 改为扁平 `{appData}/blobs/{module}/{hash}`。
- **理由**：内容寻址的 hash 已全局唯一，月份分层对 < 万级规模无收益，反而使删除、孤儿 GC、去重的目录遍历复杂度上升；且现有实现已是扁平结构，改规范比改代码更符合"最小必要复杂度"。
- **影响**：若将来单模块 blob 超过十万级，再引入两字符前缀分片（`{hash[0:2]}/{hash}`），届时作为新决策追加。

### D-07 截图捕获 v1 以 GDI/PrintWindow 为主路径（改规范）

- **决策**：**修订 DESIGN §2.1/§4.2**：v1 主路径为 GDI `BitBlt`（全屏/区域）+ `PrintWindow(PW_RENDERFULLCONTENT)`（窗口），已实现的黑帧检测与"受保护内容"提示保留；`Windows.Graphics.Capture` 作为 v1.1 增强项。
- **理由**：GDI 路径已可工作且具备回退与错误提示；Graphics.Capture 需 D3D11 interop，成本高、在无 GPU/远程会话下不可用，不构成 v1 阻塞项。**但它不能替代录屏**（见 D-08）。

### D-08 录屏（P7）移出 v1，列入 v1.1

- **决策**：**修订 DESIGN §3**：P0 的"截图与录屏"在 v1 收敛为**仅截图**；录屏列入 v1.1，前置依赖为 `Windows.Graphics.Capture`（或 DXGI）视频流 + FFmpeg sidecar + O5 的 sidecar 下载与校验通道。
- **理由**：录屏是 v1 中唯一整块能力缺席（`crates/screenshot-core/src/module.rs:6-9` 自述独立里程碑），而 FFmpeg sidecar 分发依赖尚未实现的 O5 统一下载通道；硬塞进 v1 会连带引入未就绪的 sidecar 基础设施。
- **必须同步动作**：修订文档的同时**修正 UI 承诺**——导航与副标题中"截图与录屏"不得暗示录屏已可用，v1 只能呈现截图能力（避免第二次"承诺未兑现"）。

### D-09 OCR v1 单引擎 + 保留引擎抽象扩展点

- **决策**：分两步。
  1. **补骨架（批次 2，低成本）**：实现 DESIGN §4.3 要求的 `OcrEngine` trait 与引擎注册表，把现硬编码的 `win-ocr`（`crates/ocr-core/src/module.rs:93-99`）改为注册表中的一项；**接线 `screenshot.ocr_requested` 事件**（现仅注册未消费），使"截图联动 OCR"闭环可用。
  2. **改规范（v1.1）**：PaddleOCR sidecar 与翻译引擎移出 v1（依赖 O5 下载通道与额外语言模型分发）。
- **理由**：Windows.Media.Ocr 为真实 WinRT 调用且带可操作降级提示（`crates/win-integration/src/ocr.rs:64-71`），单引擎在 v1 可用；但把引擎写死会让 v1.1 引入 PaddleOCR 时改动面过大，故骨架必须先补。
- **实施记录（2026-09-19，第 1 步）**：`ocr-core/src/engine.rs` 落地 `OcrEngine` trait（`id/display_name/available/recognize`，同步 + `Send + Sync`——全仓端口均为同步、阻塞边界由命令/事件层 `spawn_blocking` 承担，属对 DESIGN §4.3 async 草图的有意偏差）；`WinOcrEngine` 把原 `module.rs` 硬编码的 win-ocr 收编为注册表项；`EngineRegistry` 按 docs/impl/04 O1 算法 `pick(langs)`：preferred 引擎可用且支持则优先，否则按登记序取首个支持引擎，偏好语言无一被支持时语言回落空串（引擎按用户配置语言自选，与旧管线口径一致），全部引擎不可用报 `OCR_ENGINE_001` 并并入各引擎错误、hint 给"安装语言包或下载 PaddleOCR 引擎"；`set_preferred` 拒绝未知 id（`OCR_ENGINE_003`），preferred_engine 经既有 `config_schema/apply_config` 通道暴露（不新增 IPC）。`OcrPipeline` 改持 `&EngineRegistry`。`screenshot.ocr_requested` 接线（DESIGN O1 红线：两模块只经事件、不经函数调用）：截图 `finish` 新增 `"ocr"` 动作 → `dispatch_ocr_request` 将合成图写 `{appData}/frames/{task_id}.png`（tmp+rename 原子落盘）后发布 `{task_id, frame_ref}`——**事件只带路径引用不带 base64**，因 `forward_events` 将全量 payload 转发每个窗口，MB 级帧会淹掉 IPC；ocr-core `start()` 起消费协程（S4 watch 协作停机，同 automation-core）读帧→识别→发 `ocr.completed {source_task_id, text, engine}` / `ocr.failed`，**帧文件无论成败即删**，init 清空上次进程遗留帧目录；截图侧 `start()` 订阅 `ocr.completed` 回填历史 `ocr_text`（`parse_ocr_backfill` 纯函数要求 source_task_id+text 齐备，过滤 `ocr_copy_text` 的 `{action:"copied"}` 同主题发布）。请求为 best-effort：截图产物已落盘，OCR 请求失败仅告警不判 finish 失败。直接命令路径（`ocr_recognize` + `record_ocr_text`）本批**有意不动**，事件回填与之双写幂等（同文本 UPDATE），命令层清理归 S6 commands.rs 拆分批。已知存量限制如实记录：覆盖层 ocr 模式在 finish 入库**前**走直接命令路径，回填 UPDATE 命中 0 行（与接线前行为一致，非本批回归；事件路径在入库后触发，回填正确）。
- **完成证据（2026-09-19）**：新增 16 条回归测试——engine.rs 8 条（登记序默认/prefers 优先/preferred 不可用回落/语言回落空串/全不可用 001+hint 断言/set_preferred 拒绝/status+languages 并集/WinOcrEngine 适配 MockPort），ocr-core module.rs 4 条（**publish screenshot.ocr_requested → 消费协程识别 → ocr.completed 闭环** + 帧文件被消费删除 + 停机信道退出、坏路径/缺 frame_ref 发 ocr.failed、直接 recognize 发布、apply_config 偏好设置），screenshot-core 4 条（回填解析 2 条、`dispatch_ocr_request` 写帧+发布只带路径的事件+tmp 无残留、未初始化报 SCREENSHOT_STATE_001）；`cargo fmt --all --check` 0、`cargo clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` EXIT=0（ocr-core 17/17、screenshot-core 10/10）。

### D-11 编辑器 PDF 保持纯 Rust（lopdf）（改规范）

- **决策**：**修订 DESIGN §3 P2**，v1 采用纯 Rust `lopdf`，不引入 qpdf/mutool sidecar。
- **理由**：与 D-08 同源——sidecar 分发通道未就绪；纯 Rust 实现零外部依赖，符合"本地优先"。

### D-13 WinOps v1 交付范围收敛为 5 命令（改规范）

- **决策**：**修订 DESIGN §3 / impl/08 §7.2**：v1 交付已实现的 5 个 IPC 命令 + 43 条目录项；profiles / hosts / dns / restore_point / repair / helper_status 等其余命令列入 v1.1。
- **附加要求**：目录 id 前缀按 impl/08 规范统一（现为 `privacy_ad_id_off` 形式），并在文档中明确 HKCU/HKLM 的提权边界（现注释自述"HKLM 需提权 Helper，未实现前 catalog 全 HKCU"）。

### D-22 批次划分、执行顺序与质量门槛（流程）

- **决策**：
  1. 批次严格按 0 → 1 → 2 → 3 执行，**不跳批**；批次 0 与批次 1 不引入新功能。
  2. 每个批次完成的判定：`cargo test` 全绿（含新增验收）+ `tsc` 零错误 + CI 绿（批次 2 之后）+ 本批次对应决策的"验收"条目逐条勾选。
  3. 批次 1/2 的**每个**提交必须携带一条回归测试；安全红线类（D-04/D-05）必须含负例（错误密码、篡改密文、删除后 blob 不存在）。
  4. 任何新的规范偏离，必须先在本文档追加 `D-NN` 再改代码。

---

## 3. 安全与正确性类裁决

### D-04 剪贴板敏感数据改用 AES-256-GCM 信封加密（安全红线）

- **背景**：DESIGN §4.1/§8.5 声明"敏感数据 AES-256-GCM 加密存储"，实现为 DPAPI（`crates/clipboard-core/src/pipeline.rs:160` → `crates/win-integration/src/dpapi.rs`）；前端文案亦自称 DPAPI（`src/windows/MainWorkbench.tsx:192`）。
- **决策**：**补实现**。敏感条目改用 AES-256-GCM 加密正文，密钥采用信封结构：随机 DEK（AES-256-GCM 内容密钥）+ KEK（由 DPAPI/Windows 凭据保护）加密 DEK 后持久化；`zeroize` 内存清零；与 vault-core 复用同一套 `aes-gcm` 原语（依赖已在 workspace 中）。
- **理由**：§8.5 是数据安全红线，DPAPI 绑定当前用户/机器、不可审计且无法与端到端同步（sync）设计共存；AES-GCM 为 DESIGN 明确指定的标准原语，实现成本低（vault-core 已有可复用代码模式）。
- **拒绝的备选**：仅修改 DESIGN 承认 DPAPI——会同时削弱 §8.5 与 sync 的 E2E 前提，且"只用标准加密原语"是 GPL 合规与安全审计的对外承诺。
- **验收**：单测覆盖 加密→解密往返、错误密钥失败、密文篡改失败、锁定后密钥清零；前端文案同步改为 AES-256-GCM。
- **完成证据（2026-09-18，提交 `ee24f90`）**：信封格式 `MAGIC|u16le wrapped_len|wrapped_dek|nonce|ct+tag`，DEK 随机 32B（AAD 绑定 NFX1 魔数）、DPAPI 降级为 KEK 仅包 DEK、DEK 全程 `Zeroizing`；负例覆盖 错误 KEK / 篡改密文 / 篡改 wrapped DEK / 截断与谎报长度信封 / AAD 不匹配，另有真机 DPAPI 往返；`cargo test -p clipboard-core` 全绿；`EnvelopeCrypto::with_dpapi()` 已替换 CryptoPort 注册；ports.rs/store/commands/MainWorkbench 文案同步。

### D-05 剪贴板 blob 生命周期治理（安全红线）

- **背景（实测）**：`ClipStore::delete()` 返回 blob 路径（`crates/clipboard-core/src/store.rs:270-284`）但 IPC 层用 `??` 丢弃（`src-tauri/src/commands.rs:150-152`）；`clear()` 为纯 `DELETE`（`store.rs:286-299`）；`purge()` 同样不清理 blob；全仓无孤儿 GC。**被删除/清空的敏感内容仍以明文留在磁盘**。
- **决策**：
  1. 删除条目 = 先删主表记录，再删除对应 blob 文件；清空历史（`keep_pinned` 语义保留）执行**覆写删除**后再 unlink。
  2. 启动期执行孤儿 blob 扫描：主表中不存在的 blob 文件清理并记日志。
  3. 保留期淘汰（30 天）与上限淘汰（max_entries）路径复用同一套"删记录 + 删 blob"实现，杜绝第二处遗漏。
- **验收**：单测断言"删除后 blob 文件不存在""清空后 blobs 目录为空""人为放置孤儿 blob → 启动后消失"；`rm` 类操作不得存在未使用返回值（消除 `??` 丢弃）。
- **完成证据（2026-09-18，提交 `7741f99`）**：`delete/clear/purge` 收敛到单一 `remove_blob` 出口（clear/purge 覆写后 unlink，保留 pinned）；启动期 `gc_orphan_blobs` 扫描主表外文件；回归测试覆盖 删除即删 blob、清空保留置顶 blob、孤儿 GC、purge max_entries 删 blob 四场景；`cargo test -p clipboard-core` 17/17。

### D-10 OCR「复制全部」走剪贴板回写窗口

- **背景（实测）**：`ocr_copy_text` 经 `ScreenshotModule::copy_text` 直写系统剪贴板（`crates/screenshot-core/src/module.rs:586-596`），未置回写窗口 → 必然新增一条剪贴板记录，与 impl/04 验收"复制全部不触发剪贴板重复记录"冲突（impl/03 又写"OCR 结果发回 clipboard.captured"，两份文档自相矛盾）。
- **决策**：以 **impl/04 的验收为准**：OCR 结果写入剪贴板时置入既有 500ms 回写窗口（`crates/clipboard-core/src/pipeline.rs:24` 的 `WRITE_BACK_WINDOW`），**不产生新历史条目**；同时把 impl/03 的矛盾表述修正为"OCR 结果写入剪贴板，不回灌历史"。
- **理由**：OCR 文本是用户显式"复制"动作，回灌会让历史被 OCR 噪声污染；回写窗口机制已存在且被 `clipboard_paste` 正常使用，复用即可。
- **完成证据（2026-09-18）**：`ocr_copy_text` 改走 `ClipboardModule::write_back()`（置窗口后写端口），`ScreenshotModule::copy_text` 随唯一调用者删除；impl/03 矛盾表述改为"发 ocr.completed；复制全部经回写窗口、不回灌历史"；回归测试 `write_back_window_suppresses_self_capture` 断言窗口内事件不入库、窗口外恰入 1 条（`cargo test -p clipboard-core` 18/18）。

### D-12 同步库归位 `db/sync.db`

- **决策**：`crates/sync-core/src/module.rs:227` 的 `{appData}/sync/sync.db` 改为 `{appData}/db/sync.db`，与 O3 的"每模块独立库统一在 `db/` 下"一致；启动时若发现旧路径文件则搬移（当前无正式发布版本，兼容逻辑仅需一次）。
- **附加**：新增断言测试 `db_path.parent().ends_with("db")`，防止后续新增模块重犯。
- **理由**：`db/` 目录是备份/迁移/卸载残留清理的枚举入口，散落外部会导致漏备份与卸载残留。
- **完成证据（2026-09-18）**：`SyncModule::new` 的 `db_path` 改为 `{appData}/db/sync.db`；init 前 `migrate_legacy_db_path()` 一次性搬移旧文件（含 -wal/-shm 伴生，新库存在则绝不覆盖，旧空目录移除、非空保留）；测试 `db_path_is_under_db_dir`（父目录 ends_with("db") 断言）+ `legacy_db_moved_once_and_idempotent`（搬移/幂等/不覆盖三断言），`cargo test -p sync-core` 全绿。

### D-23 多显示器 / 混合 DPI 策略

- **背景（实测）**：覆盖层按单一 `devicePixelRatio` 线性换算物理像素（`src/windows/OverlayShot.tsx:333-340`），跨"100% + 150%"双屏时副屏选区错位；贴图居中用主屏 `window.screen.width`（`OverlayShot.tsx:765-770`），多屏必然落在错误显示器。impl/03 P3 原要求"每显示器一个覆盖窗口 + 物理像素坐标"。
- **决策**：
  1. **批次 1（低成本必做）**：贴图/覆盖层定位改用 `currentMonitor()`（含 `position` 与 `scaleFactor`）取代 `screen.width`；移除硬编码偏移。
  2. **v1.1**：按 `availableMonitors()` 为每块屏创建独立覆盖窗口，各自使用本屏 `scaleFactor` 换算 —— 这需要重做覆盖层生命周期，与批次 1/2 无关，单独立项。
- **理由**：定位错屏是"明显错误"，成本极低必须先修；每屏多窗口是架构级改动，不应阻塞安全红线批次。
- **完成证据（2026-09-18，批次 1 范围）**：贴图居中改用 `currentMonitor()`（截图覆盖层所在显示器的物理矩形，position + size 直算），移除 `screen.width × dpr` 主屏基准换算与硬编码；crop 物理宽高原样使用不再乘 dpr；`npx tsc --noEmit` 零错误。第 2 条（每屏独立覆盖窗口）按裁决留在 v1.1。

---

## 4. 工程质量与体验类裁决

### D-14 引入 Zustand 三 store

- **背景**：DESIGN §7 与 UI-PLAN §1 均将 Zustand 列为技术基线，实测 `package.json` 无该依赖；状态以 `useState` 分散在 25 个文件，导致：切模块重复拉取 4 个 IPC、跨窗口主题不同步、`modules.ts` 的 `running: true` 为静态字面量（导航绿点与 StatusBar 真实状态矛盾）、`StatusBar` 2s 轮询与 `QuickPanel` 1.5s 轮询无法移除。
- **决策**：引入 Zustand，建三个 store：`session`（主题/活跃模块/分组/搜索，`persist` 到 localStorage）、`modules`（由 `nf:event` 驱动的模块状态）、`notifications`（错误与 toast）。
- **验收**：删除 `StatusBar` 与 `QuickPanel` 的轮询；`modules.ts` 不再含 `running` 字段；主题切换跨窗口实时同步。
- **实施记录（2026-09-18）**：`src/stores/modules.ts`——`useModuleStatus`（id→ModuleState 映射）+ `startModuleStatusFeed()`（幂等：初值取 `host_modules_status` 快照，此后仅靠既有 `nf:event` 转发中的 `host.module_state` 事件直推，零后端改动）；`ModuleNav` 运行点与 `StatusBar` 健康点改读该 store，未收录模块回退显示阶段标签，浏览器预览（states 空）回退静态 4 模块展示。`src/stores/session.ts`——`themeMode/activeModule/clipGroup` 经 `persist` 入 localStorage（`clipSearch` 为会话内易失态，`partialize` 有意排除）；实测 zustand v5 的 persist **不监听 storage 事件**（原语"persist 自动跨窗口同步"不成立），补 storage 监听 → `useSession.persist.rehydrate()` 桥接实现跨窗口实时同步；OS 深浅色变化另由 main.tsx 的 matchMedia `change` 监听器逐窗口响应（`resolveIsLight` 仲裁 auto）。`notifications` store 已随 D-19 落地，即第三 store。轮询删除：`StatusBar` 2s `host_modules_status` 轮询、`QuickPanel` 1.5s `clipboard_search` 轮询（改为初载一次 + `clipboard.captured/deleted/cleared` 事件直推；窗口常驻隐藏期间事件仍更新列表）。`layout/modules.ts` 的 `running: true` 静态字段连同 14 处字面量删除。
- **完成证据（2026-09-18）**：`npx tsc --noEmit` 与 `npm run build` 零错误；全仓 `setInterval` 仅剩 VaultPanel 两处倒计时 UI 定时器（非状态轮询）；浏览器双窗口实测（vite dev + 同源第二窗口）：① 导航点击 → localStorage `nf-session` 立即写入 `activeModule:"screenshot"`；② 从窗口 B `setItem` 置 `themeMode:"light"` → 窗口 A（OS 模拟仍为 dark）FluentProvider 背景实时由 `#1c1f26` 翻至 `#eef2f7`，主题切换跨窗口同步达成；③ emulate 深浅色切换单窗口内热替换（matchMedia 监听器生效）；④ 状态栏回退态与导航阶段标签在 states 空时正常渲染，console 无错误（仅 favicon 404）。

### D-15 锁策略统一（parking_lot + 回调无锁快照）

- **背景**：静态扫描统计 `.lock().unwrap()/expect()` **约 186 处**；其中运行期高风险点包括事件总线（`crates/host-core/src/events.rs:136-166`，poison 后全模块发布崩溃）、剪贴板连接锁（14 处）、代理还原路径（`crates/proxy-core/src/sysproxy.rs:153-160`）、以及**在低级输入钩子回调内取锁**（`crates/win-integration/src/input.rs:111`，阻塞会导致系统级输入卡顿）。
- **决策**：全局改用 `parking_lot::Mutex/RwLock`（无 poison 语义）；输入钩子的回调改用 `ArcSwap` 无锁快照，**禁止在 hook 回调内获取任何 std 锁**。
- **理由**：poison 会让 DESIGN §8.2 的"模块 Error 可重启"变成"重启后首个 `lock()` 立即再 panic"，属实质功能缺陷而非风格问题；`parking_lot` 替换成本低、无 API 破坏。
- **实施记录**：56 个文件、全部 std `Mutex/RwLock` 换为 `parking_lot`（tokio `AsyncMutex` 保留——本无 poison 语义；`nexusforge-helper` 无锁未涉及）。186 处 `.lock().unwrap()/expect("…锁污染")` 全部消失；衍生清理：32 处 `*self.X.write().map_err(|_| ModuleError::Init("锁污染"))? =` 端口注入样板、perf.rs 四个 `SYS_PERF_003 锁污染` 错误路径（parking_lot 下取锁不可能失败，改为直接 `Ok(...)`）、notes/sync/desktop 的 `.read().ok().and_then(|g| g.clone())` Option 链改 `.read().clone()`。钩子回调（`keyboard_proc/mouse_proc`）改 `ArcSwap<Option<Cb>>::load()` 无锁快照，`start_capture/stop_capture` 经 `store()` 发布/清除；钩子线程自身的 `thread_id/req_rx/hooks` 仍用 parking_lot Mutex（非回调路径）。
- **完成证据**：`cargo fmt --check` 0、`cargo clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` 45 套件全绿。回归测试两枚：`events.rs::publish_survives_panic_under_lock`（持总线写锁的线程 panic 后发布/订阅仍正常——std 语义下此测试必失败，正是 poison 级联缺陷的正面证据）；`input.rs::hook_cb_visible_via_lockfree_snapshot`（start 后回调经 ArcSwap 立即可见、stop 后清除）。脚本误伤两处非锁同名方法（`SysProxyPort::read()` trait 方法、`VaultService::lock()` 领域方法）已由编译器全部捕获并恢复 `.unwrap()`，最终 `grep` 复核：残留 12 处 unwrap 均为这两类领域方法而非锁。

### D-16 host-core 收敛公共工具与 ModuleStateCell

- **决策**：`now_ms()`（现 9 份）、错误工厂（8 份）、`hex`/`b64`（4 份）、模块状态机（13 份逐字复制）统一收敛到 `host-core`（`util` + `ModuleStateCell`）；`Module::status()` 直接返回统一状态，消除"模块内 AtomicU8"与"注册表 HashMap"两套状态源。
- **附加**：`ModuleInfo.priority` 的硬编码 `10` 改为集中优先级表（DESIGN §8.3 要求按优先级仲裁快捷键冲突，散落常量使仲裁不可审计）。
- **实施记录（2026-09-18）**：实测漂移比审查更大：`now_ms`/`unix_ms` 具名定义 **15 份**（含 kvm `unix_ms` 与 proxy/helper 的 u64 变体）另加 proxy `kernel.rs` 内联拷贝 1 处，全部改 `host_core::util::now_ms`（i64）/ `now_ms_u64`（u64 协议字段），全仓 `fn now_ms` 仅剩 util 一处（文件系统 mtime→epoch 换算非"当前时间"，保留）。错误工厂：9 处 `AppError::module(code, msg, None)` 型逐字复制（screenshot util/store/module、ocr pipeline/module、win-integration capture/ocr、vault）改 `use host_core::util::app_err as err`，不同 kind 的工厂（clipboard `Storage`、host `cfg_err`、win32 HRESULT、io 映射）语义不同保留。**顺带修复真实缺陷**：clipboard `module.rs err(_code, m)` 吞掉错误码——现以 `[CODE] message` 嵌入 `ModuleError::Init`。hex/b64：`util` 提供 `hex_lower`/`b64_encode`/`b64_decode`/`b64_decode_lenient`（URL_SAFE_NO_PAD 优先、STANDARD 兜底），收敛 clipboard `hex`、proxy `sha256_hex` 与订阅双引擎解码、kvm `hex_str`、vault crypto `b64/unb64`、screenshot `png_b64` 对、ocr `decode_rgba`；`device.rs` b64 保留 re-export 兼容既有导入。状态机：新增 `ModuleStateCell`（AtomicU8，0=Uninit/1=Stopped/2=Running/3=Error，未知值 fail-safe 归 Error——原 12/14 模块 `_ => Running` 一并消灭），14 个模块 + 3 个宿主测试 mock 全部换格；`Module` trait 增 `set_status`，注册表**删除 `states: HashMap` 第二状态源**，`status_all`/panic 补记 Error 均直写模块 cell，且 `host.module_state` 事件从"仅 start 发布"改为 **init/stop/panic 全迁移发布**（D-14 前端状态流此前在两区间失真）。优先级：`priority_of(id)` 集中表落 host-core（10×3/12/13×2/14/15×2/16/20×3/25 与散落字面量实测一致，非审查记载"全 10"），14 模块 `info()` 改表推导；kvm 具名魔数常量删除。附带修复：sync-core 4 处测试临时库路径以 `process::id()` 去重，Windows PID 复用导致跨运行残留数据（engine/oplog 5 测试随机失败），改 `uuid::now_v7` 与 `tests/loopback.rs` 既有做法对齐。
- **完成证据（2026-09-18）**：`cargo fmt --check` 0、`cargo clippy --workspace --all-targets -- -D warnings` 0、`cargo test --workspace` 45 套件全绿（EXIT=0）。回归测试 8 枚：`util` 4（now_ms epoch 合理性与单调、hex 已知向量、b64 双字母表往返与垃圾输入、app_err 保码）；`module.rs` `state_cell_roundtrip_and_unknown_failsafe`（未知魔数 9 必归 Error）、`priority_table_covers_all_shipped_modules`（14 模块逐名登记 + 默认值 + 仲裁序断言）；`registry.rs` `registry_and_module_share_single_state_source`（panic 后注册表视图==模块视图==Error，旧双源实现下必失败）、`module_state_events_cover_init_and_stop`（init→Stopped、start→Running、stop→Stopped 三事件齐发，旧实现只发 start）。

### D-17 前端工程门禁（ESLint + Vitest + CI）

- **背景**：`package.json` 仅 `tsc && vite build`；无 ESLint/Prettier/Vitest/Testing Library/Playwright；UI-PLAN §4 要求"eslint 零告警"、U8-1/U8-2/U8-3 三项无产出。
- **决策**：ESLint（`react-hooks` + `jsx-a11y`）先落地——它可自动捕获本次发现的"行不可聚焦""点击无键盘等价"类问题与 `exhaustive-deps`；Vitest 首批测试聚焦纯逻辑：`parseAppError`、`SchemaForm` 默认值合并、剪贴板筛选/分页参数构造。Playwright 视觉基线列入 v1.1。
- **验收**：`npm run lint` 与 `npm run test` 进入 `ci.yml`；现存 7 处 `eslint-disable-next-line react-hooks/exhaustive-deps` 逐条复核。
- **实施记录（2026-09-18）**：ESLint 9 扁平配置（typescript-eslint 8 + eslint-plugin-react-hooks 7 + jsx-a11y），`no-empty` 关闭空 catch 豁免作为 D-19 兜底；react-hooks v7 随附的 6 条 React-Compiler 系规则（set-state-in-effect 等）超出本裁决范围，逐条关闭并注明留待独立立项。jsx-a11y 静态检查看不见 spread 属性，故键盘等价采用**字面属性**模式：`role="button" tabIndex={0} onKeyDown={keyActivate(fn)}`（`src/a11y.ts`），覆盖剪切板/笔记/终端（会话页签、kill、SFTP 行）/vault/启动器/快查面板 8 类行元素；不可键盘化的装饰面（遮罩、十字准星画布、贴图缩放手势面）标 `role="presentation"`。2 处 `no-autofocus` 豁免（启动器/速记条为呼出式窗口，出现即聚焦是核心交互）。Vitest 3（jsdom 环境，锁 v3 因 v5 要求 Vite≥6 与项目 Vite 5.4 冲突）+ 19 条首批测试（parseAppError 7 / mergeDefaults+toFiniteNum 9 / clipSearchParams 3）。
- **7 处 exhaustive-deps disable 逐条判定**：3 处为陈旧遗留（依赖实际早已补全，豁免纯属僵尸）直接删除——EditorPanel 挂载 effect、OverlayShot `[crop, task]` effect 等；1 处修复：OverlayShot `setupCanvas` 本为 `useCallback(…, [])` 稳定引用，直接入依赖表并去掉豁免；余 3 处保留并就地写明原因（OverlayShot keydown：undo/redo 为每渲染新建的 ref-only 函数，入表即每帧重挂监听且行为不变；OverlayShot confirmSelection：runOcr 是其后置 `const`，TDZ 无法入表；EditorPanel Ctrl+S：doSave 同理每渲染新身份，activeId 已在表内保证语义正确）。3+1+3=7，逐条清零。
- **完成证据（2026-09-18）**：`npx tsc --noEmit` 0 错误、`npm run lint` 0 告警（全仓首次）、`npm run test` 19/19。门禁顺带揪出并修复真实缺陷：SchemaForm `Number(x) ?? 0` 死空值合并（NaN 泄漏进 SpinButton，抽出 `toFiniteNum` + 6 条单测锁行为）、TerminalPanel SFTP 路径拼接双侧同值的死三元。

### D-18 前端组件基线 + 破坏性操作统一二次确认

- **背景**：UI-PLAN §3 规划的 `components/{EmptyState,Toast,VirtualList,HotkeyHint}` 一个都没建；`.section` 卡片样式在 9 个面板各写一遍、tab 胶囊 5 份、`.error/.ok` 12 份；删除类操作（凭据条目、文件夹、笔记、规则、插件、KVM 配对、订阅、清理、桌面整理）普遍单击即执行。
- **决策**：建 `src/components/`：`Section`、`Tabs`、`InlineError`、`EmptyState`、`ConfirmDialog`（基于 Fluent `Dialog`，自带焦点陷阱与 Esc）；**所有破坏性操作必须经 `ConfirmDialog`**，展示影响面（条目数/文件数）；`SysPanel` 的"命令预览确认"作为推广样板。
- **附带收益**：一次性解决对话框 a11y（当前 `VaultPanel` 对话框无 `role`/无焦点陷阱/Esc 失效）与三态不一致（5 个面板首屏"假空态"）。
- **实施记录（2026-09-19）**：`src/components/` 五件套落地——`Section`（统一卡片：Stroke1 边框/radiusLarge/12px 16px 内距/NeutralBackground1，可选标题行 + 动作槽）、`Tabs<T>`（`role=tablist` 容器内渲染真实 `<button role=tab aria-selected>`，规避 jsx-a11y 看不见 spread 属性的问题）、`InlineError`（error=红 Base200 + `role=alert aria-live=assertive`；success=绿，供面板复用的成功行）、`EmptyState`（`loading` 参数区分"加载中…（role=status）"与真空态文案，根治首屏假空态）、`ConfirmDialogHost`（基于 Fluent `Dialog` 复合组件，自带焦点陷阱/Esc/遮罩点击）。确认通道为 `src/stores/confirm.ts`：zustand FIFO 队列 + 模块级 `Map<id, resolver>`（resolver 不入 store 保可序列化），`confirmAction(opts): Promise<boolean>` 支持 `impact: string | string[]`（条目数/文件数影响面）、`command`（等宽预览块 + 「确认执行以上确切命令？」，即 SysPanel 样板泛化）、`danger`（红 token 覆写主按钮——装机的 Button 联合类型无 `overflow` appearance，`borderColor` 单独设置会类型报错，统一用 `border` 简写）。Host 挂载于 `main.tsx` FluentProvider 内与 Toaster 并列，覆盖全部 `?w=` 窗口角色；Esc/遮罩 = `settleHead(false)`。13 个面板 + 4 个窗口/布局文件迁移：SysPanel 删除本地 pending 状态与命令预览条改走对话框（安装类 danger=false、卸载/清理 danger=true，回收站模式以文案提示可恢复性），NotesPanel 删除笔记（反链悬空警告）/重命名（出链/反链计数）/删卡/删画布节点（名称 + 边数）全部门控，VaultPanel 手写 `dialogScrim` 模态整体换 Fluent Dialog（条件挂载 + `onOpenChange` 走异步关闭守卫），编辑器脏检查抽为纯函数 `serializeEditorForm`/`isEditorDirty`（`initial` 快照字段，生成器参数不计入脏态）并配 5 条 Vitest 回归；条目删除门控（字段数 + TOTP 行），文件夹删除以 `vaultEntries(folderId).length` 客户端预取影响数（DTO 无计数字段），修复"双击=删除"伪确认与删除当前活动文件夹后视图失焦两个存量 bug。代理组核查结论：KvmPanel 解除配对门控（设备名/指纹/配对设置），「切回本机」有意不门控（fail-safe 方向）；DesktopPanel 修复 success 文案误入 error state 的存量 bug 后 tidy/restore/便签删除门控；TerminalPanel kill 会话（标注 local/WSL/SSH 类型）与 docker stop 门控；FilePanel 删除（目录/文件计数 + 前 3 名称 + 回收站措辞对照 `state.rs:128` RecycleBinPort 实现核实）、冲突「全部覆盖」门控，`pendingSpec` 携带 `srcs` 使确认后作用于确认时的文件集；EditorPanel 脏文档关闭（放弃并关闭，已核实 close 删除 autosave 草稿 `editor-core/session.rs:245`）、PDF 压缩/水印就地覆盖门控、挂载补 `editorSessions()` 调用修复冷启动空白；ClipboardPanel 删除门控（敏感条目以「敏感条目（加密信封）」占位，绝不回显内容）并将本地 toast 收敛到 `reportError`/`notify`；SyncPanel/ProxyPanel（订阅删除带 node_count）/RulesPanel（规则/插件删除带引用计数）/QuickPanel 三态与 Section 统一；PinWindow 经核查**不加**确认——贴针 PNG 在窗口创建前已持久化且 `pin_update` 即时落盘，不存在未保存状态，`pin_close` 删 PNG 为设计意图（客户端无法得知用户是否要保留，已注明为 D-18 核查备注）；OverlayShot 编辑阶段「重选/Esc」单一 `discardToSelect()` 门控，仅当已有标注时触发，影响面按工具列出标注数/重做数/裁剪尺寸，并以 `confirmPending` ref 屏蔽全局 keydown（Fluent `useDialogSurface` 的 Escape 处理先于窗口监听，属真实竞态修复）；StatusBar 移除 `Del 删除 · Ctrl+P 置顶` 虚假提示（处理器不存在，已核实）。共享组件缺口如实记录（未强行套用）：FilePanel 变体着色/滚动容器卡片与 Editor/Terminal 带脏点+关闭按钮的页签条无法用 `Tabs`/`Section` 表达，保留局部样式。
- **完成证据（2026-09-19）**：全仓 27 处 `confirmAction` 调用点（grep 计数，12 个面板/窗口文件）；`npx tsc --noEmit` 0 错误、`npm run lint` 0 告警、`npx vitest run` 5 文件 30 测试全绿（含 confirm 队列 FIFO/结算/空队列幂等 6 条 + 编辑器脏检查 5 条回归）、`npm run build` EXIT=0；浏览器实测（vite 5199 全新 optimize-deps）：ConfirmDialog 打开/影响面/命令预览/焦点陷阱/Esc 结算 false/确认结算 true/队列清空，工作簿首屏、ClipboardPanel 三态、StatusBar 提示修正、SysPanel Tabs `aria-selected` 均正常；Vault 面板在无 Tauri 后端的浏览器中以内联 InlineError 呈现错误（非崩溃，React 树完好），NotesPanel Tabs/EmptyState 正常渲染。会话中途遇到的 `504 Outdated Optimize Dep` 动态导入失败经定位为上一轮测试遗留 dev-server 的过期依赖缓存（换 `--force` 全新服务后消失），非代码缺陷。

### D-19 全局错误通道与模块重启入口

- **背景（实测）**：`AppErrorDto` 已带 `hint`/`retryable`，但全库仅 1 处渲染 `hint`；无 `Toaster`/`Dialog`；空 `catch` 块实测 11 处、含其他静默形式合计 40+ 处（示例：`src/modules/vault/VaultPanel.tsx:396` 删除失败被吞后照常 `refresh()`）；`hostModuleRestart` 有定义无调用者，`StatusBar` 的 Error 红点不是按钮——DESIGN §8.2「UI 显示"该模块已停止，点击重启"」落空。
- **决策**：建 `notifications` store + 全局 `Toaster` + 统一 `reportError(e, opts)`；`StatusBar` 红点改为按钮并调用 `hostModuleRestart(id)`；40+ 处静默 catch 一律替换为"记录 + 状态栏角标"，禁止裸 `catch {}`（由 D-17 的 ESLint 规则兜底）。
- **实施记录（2026-09-18）**：`src/stores/notifications.ts`——zustand store（notes 历史 30 条 / 同屏 toast ≤4 / `unseenErrors` 角标）+ `reportError(e, {context, dedupeKey, toast})`：normalize `AppErrorDto`（含 `hint`；非 AppError 走 humanize 兜底 `JSON.stringify`）→ 宿主日志必有 → 角标必有 → toast 默认；`toast:false` + `dedupeKey`（3s 窗口）用于轮询、每帧 ack、监听注册等高频低价值路径；main.tsx 挂全局 `unhandledrejection`/`error` 兜底。`src/components/Toaster.tsx` 按 demo 规格（右 16px/底 40px、320px、error 6.5s 自动消失、`role=status`/错误条目 `role=alert`、zIndex 1000 高于 vault 模态），挂载于 `Root()` 内与 `<App/>` 并列，覆盖所有 `?w=` 窗口角色。StatusBar：Error 态模块渲染为按钮（§8.2 文案"该模块已停止，点击重启"）调 `hostModuleRestart`（stop→init→start 由宿主执行），结果 toast 反馈；新增"N 条错误"红角标，点击回放最近错误并清零。全仓 32 处 `catch(() => undefined)`/`catch(() => {})` 与 11 处注释吞异常站点全部改接 `reportError`；有意保留的静默仅 3 处：client.ts `hostLog` 防递归 guard、`hostSystemAccent`/`hostModulesStatus` 两处 by-design null 返回；面板内 `setError` 内联错误带保留（非静默）。U7-1 焦点感知双通道（Windows 系统通知）延后至批次 3；D-17 ESLint `no-empty` 兜底规则随批次 2 落地。
- **完成证据（2026-09-18）**：`npx tsc --noEmit` 与 `npm run build` 零错误；浏览器实测（vite dev + DevTools 脚本注入）：dispatch 伪造 `AppErrorDto` rejection → toast 呈现 `[DEMO_003] 演示错误：超时`、状态栏出现"1 条错误"角标并随第二条累加至 2 → 点击角标回放最近 3 条错误 toast 且角标清零。Error 态真实重启（U7-3"mock panic 场景可从 UI 重启"）为 GUI 手测项，随批次验收执行。

### D-20 ConPTY 诊断探针与验收门禁治理

- **背景（实测）**：`cargo test --workspace` 返回 EXIT=101，失败用例为 `crates/win-integration/tests/conpty_min.rs:154` 的 `conpty_min_echocon_reference`。该文件**未提交**（`git status` 显示 `??`）却因位于 `tests/` 而默认参与测试；它还调用 `FreeConsole()` 摘除调用方控制台，污染运行它的终端与同进程后续测试。另 `conpty_acceptance.rs:10` 的 10k 行验收为 `#[ignore]`。
- **决策**：
  1. 诊断探针移出测试门禁（加 `#[ignore]` 并注明用途，或移入 `tools/` 目录），并移除 `FreeConsole()` 这类对宿主进程的副作用；
  2. 恢复"`cargo test --workspace` 一条命令全绿"——**这是 CI 的前置条件**；
  3. ConPTY 在本机系统 conhost 路径产不出渲染流（三套独立实现一致失败，IDE 内嵌终端正常）登记为**已知环境限制**，term 模块的终端可用性以 GUI 宿主验收为准；若 GUI 宿主同样失败，则启动"内置 OpenConsole/conpty.dll sidecar"子项（v1.1）。
- **验收**：`cargo test --workspace` EXIT=0，且忽略用例仅保留有明确理由的 ConPTY 类。
- **完成证据（2026-09-18）**：实测 `cargo test --workspace` EXIT=0、失败 0；ignore 收敛为 3 个 ConPTY 用例（`conpty_acceptance` 10k 吞吐、`conpty_min`/`conpty_portable` 诊断探针，均注明用途与运行方式）；探针保留在 `tests/` 但默认不入门禁，`FreeConsole()` 副作用限定在 `--ignored` 手动复现场景；分诊定论的 `conpty.rs` 两处时序修复（句柄在 CreateProcess 后关闭、watcher 等读端 EOF）已随本批合入。

### D-21 仓库治理与开源合规补齐

- **决策**：
  1. `.gitignore` 删除全部 Python 段落（技术栈已定 Rust + TS；其中 `lib/`、`*.spec` 有误伤风险），保留 Node/Rust/OS/IDE 段并去重 `dist/`；
  2. 补齐 **`LICENSE`（GPL-3.0 全文）** 与 **`THIRD_PARTY_LICENSES.md`**（DESIGN §7 与项目硬约束均要求，当前双双缺失）；
  3. 重写 `README.md`——现文档仍是 Python 时代内容（`pip install -r requirements.txt`），与 Tauri/Rust/React 实现完全不符；
  4. `tauri.conf.json` 的 `pubkey` 占位符在发布前替换为真实签名公钥，`targets` 由 `["nsis"]` 扩为 `["nsis","msi"]`。
- **理由**：合规文件缺失是**对外承诺**问题（仓库声明 GPL-3.0 却无许可证文本），优先级高于功能补齐。
- **完成证据（2026-09-18）**：`.gitignore` 已删全部 Python 段（含 `lib/`、`build/`、`*.spec` 误伤项）；`README.md` 重写为 Rust/Tauri/React 口径；`LICENSE` 为 gnu.org 官方 GPL-3.0 全文（35,149 字节）；`THIRD_PARTY_LICENSES.md` 由 `tools/gen-third-party-licenses.mjs` 从 `cargo metadata` + `package-lock.json`/node_modules 生成（Rust 552 + npm 221，无未声明项、无 GPL 传染项，MPL-2.0/CC-BY-4.0 属兼容弱许可）。updater pubkey 与 MSI target 按 D-21 第 4 条留待发布前（批次 3）。

---

## 5. 明确不做（v1 范围外，已记录不再重开）

| 项 | 原因 | 处置 |
|----|------|------|
| 国际化（i18n） | DESIGN 未要求；出货范围是简体中文桌面工具。安装器提供 English 但 UI 为中文的割裂感已知 | 不立项；先把 `MainWorkbench` 的 14 层嵌套三元抽为 `MODULES[].subtitle` 常量表，为将来留唯一落点 |
| 每显示器独立覆盖窗口 | 架构级改动，见 D-23 | v1.1 |
| 同步 3-way 合并 / 中继服务器 | 与"本地优先、服务端不见明文"的定位冲突 | v1.1+，需新决策 |
| 端口转发 / 跳板机（T4） | 依赖 SSH 会话复用重构 | v1.1 |
| 网盘驱动（SMB/FTP/WebDAV/S3，经 rclone） | 依赖 sidecar 分发通道（D-08 同源） | v1.1 |
| WASM 插件市场签名与分发 | 需后端与信任根设计 | v1.1+ |
| 性能基准（criterion）与启动/内存/搜索阈值断言 | 门禁体系（D-01/D-17）就绪后才有意义 | 批次 3 |

---

## 6. 决策变更流程

1. 任何新的偏离或取舍，先在本文件追加 `D-NN`（含背景/决策/依据/代价/验收五要素），再动代码；
2. 涉及 DESIGN.md 条文的，同步登记 DESIGN.md §11 修订表；
3. 已裁决条目**不删除、不重排**；被推翻时追加新编号并标注"取代 D-x"；
4. 条目状态由 `待实施 → 实施中 → 已完成` 单向推进，完成后在验收列标注实测证据（命令与输出）。