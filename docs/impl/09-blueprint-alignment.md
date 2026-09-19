# 09 蓝图对齐 · 全模块深化总方案（D-29）

> 底稿地位：本文件是 `docs/DECISIONS.md` D-29 的唯一实施底稿。**流程规则（用户裁定，2026-09-19）：每一补深批次开工前，先在本文件追加/修订对应小节方案，再动代码；单节容量不足时拆独立子方案文档并在此挂链。**
> 总纲目标（用户原话口径）："全部功能面板根据功能做最深度的设置，尽可能实现参考软件的全部功能，而不是主要功能。"
> 蓝本来源：《集合软件开源蓝本》（DeepSeek 导出 2026-09-14）§3.1–§3.12；参考软件按节标注。
> 状态标注：`未开工 → 进行中 → 已完成(+commit)`，与 §12 跟踪表同步。
> **逐面板细案（2026-09-19 增）**：每个功能面板的问题清单（file:line）、子面板信息架构、对标全功能矩阵、其他软件借鉴与拓展设计，见 [`docs/panels/2026-09-19/`](../panels/2026-09-19/README.md)（README 含信息架构总则与五类子面板规范；15 份分档与本文件批次号互相引用，细案先于代码）。

## 1. 差距总览与三分法

事实底账来自两次只读深度勘查（12/12 面板区块与 IPC 清单、177 命令全量分组、7 crate 能力面），差距分三类：

- **[壳缺]** UI 组件不存在：screenshot、ocr 两模块无主面板（命中"模块界面待实现"兜底）；设置中心硬编码 `moduleId="clipboard"`。
- **[收窄]** 后端能力被静默裁剪且从未登记：代理单内核（`proxy-core/src/service.rs:463` 硬编码 `SingBoxDriver`、`config.rs` 无中间 IR、`sub.rs` 仅 4 协议枚举）；办公助手全链无踪迹。→ 已由 D-29 补登记。
- **[孤儿]** 后端已实现、前端零入口（约 30 命令，全表见 §4）。

| 模块 | 蓝本节 | 参考软件 | 现状判定 | 批次 |
|------|--------|----------|----------|------|
| 剪切板 | §3.1 | Ortu（+EcoPaste/CopyQ 亮点） | 捕获/FTS/加密/去重厚；缺粘贴堆栈、智能分组、掩码揭示、导出导入 [收窄] | B3 |
| 代理 | §3.2 | v2rayN/IRBox/FlowZ | **单内核、配置方言强耦合、订阅 4 协议、TCP-RTT 伪测速** [收窄] | B2 |
| 密码库 | §3.3 | Vaultwarden | 后端全（改密/重命名/TOTP/Hello）；UI 缺 4 入口 + 搜索恒 null [孤儿] | B1 |
| 文件 | §3.4 | Oxyde_FM | 引擎厚（队列/断点/冲突/USN/重命名 DSL）；7 命令无 UI，副标题已对外承诺 [孤儿] | B1 |
| 网盘 | §3.5 | OpenList/Alist | 仅注释零实现（rclone 扩展位）→ **§5 推翻，D-29 提前** | B6 |
| FTP/WebDAV/HTTP | §3.6 | AeroFTP/Filestash | 同上，`TransferProtocol` 抽象不存在 | B6 |
| 键鼠共享 | §3.7 | Lan Mouse/Deskflow | 通道/配对/回写厚；send_clip/send_file/会话表无 UI，XButton/水平滚轮/上下缘缺 [孤儿+收窄] | B7 |
| 更新卸载 | §3.8 | UniGetUI | PkgManager 三源适配在；动作仅 install/uninstall/upgrade_all，无仓库搜索/单包升级 [收窄] | B7 |
| 截图 | §3.9 | ShareX/OpenSnap/Ksnip | 7 工具+撤销重做+贴图在；无图层/滚动截/美化/上传流水线；**主面板 [壳缺]** | B0+B4 |
| OCR | §3.10 | Umi-OCR/Paddle/Tesseract | WinOCR 单引擎、EngineRegistry 扩展位在；无批量/语言选择/翻译槽；**主面板 [壳缺]** | B0+B4 |
| 终端 | §3.11 | NyaTerm | ConPTY/WSL/SSH/SFTP/Docker 在；known_hosts 管理无 UI、端口转发/ProxyJump 缺（§5 推翻提前）| B7 |
| 系统监控 | §3.12 | NeoHtop | 采样环+3 曲线在；无进程 top-N/结束进程、托盘负载；winops 目录浏览无 UI [孤儿] | B7 |
| sync | §五(整合) | Syncthing 理念 | **后端缺可查询接口**（per-peer 游标/冲突历史/自动同步/entity 白名单写死 "note" `module.rs:39`）——非纯 UI 问题 | B5 |
| editor/notes/desktop/automation | — | — | 各有半截功能（细案复核：notes 18 命令**零孤儿**、其薄在 textarea 编辑形态与信息架构；editor 孤儿=editor_save_as+草稿箱只写不看；automation UI 单动作表单会裁切多动作规则、IpcCommand/and-or-not 无入口——见 panels/11/12/13）| B1+B7 |

## 2. 批次划分与依赖

| 批次 | 内容 | 依赖 | 规模档 | 状态 |
|------|------|------|--------|------|
| B0 | UI 壳补齐：截图/OCR 主面板、设置中心 moduleId、MainWorkbench 注册表化、占位 Badge 机制 | 无 | M | 未开工 |
| B1 | §4 孤儿命令接线全表（纯前端 + 少量 DTO 补齐） | B0（注册表化先行） | L–M/面板，可切多提交 | 未开工 |
| B2 | 代理多内核（§5） | B1 中代理相关项 | XL | 未开工 |
| B3 | 剪贴板深化（§8） | B1 | L | 未开工 |
| B4 | 截图/OCR 深化（§9） | B0/B1 | XL | 未开工 |
| B5 | sync 深化（§10，含后端接口） | 无 | L | 未开工 |
| B6 | 文件/网盘多协议（§6） | B1 file 接线、D-28 分发通道 | XL | 未开工 |
| B7 | 终端等七模块深化（§7） | B1 | XL（拆 7 子批） | 未开工 |
| B8 | 办公助手立项、i18n 复审——**只出方案等裁决，不写代码**（§11） | — | 方案 | 未开工 |
| B9 | 构件与分流规则自动更新治理（内核/geo/订阅双轨，三态策略+手动下载）——**码级任务书已独立成文：[10-artifact-auto-update.md](10-artifact-auto-update.md)**（D-32 预留） | B9a 无依赖；B9b（xray/mihomo/rule-set 收编）随 B2 | L | 未开工 |

顺序理由：B0 消除"模块待实现"这类最刺眼观感；B1 以最小代价兑现最多"面板里详细内容"；重架构（B2/B6）不与壳层改动同批，避免回归面重叠；B9a 是纯增量框架（新 crate+新命令域），与 B0/B1 无文件交叠可并行排产，B9b 挂 B2 尾部。

## 2.5 任务书粒度标准（五审裁定："文档内容要做到代码级"）

每个实施批次开工前，其 09 小节（或独立方案文档，如 10）必须把每个任务写满六栏，缺一栏不算任务书：

1. **目标锚点**：文件:行 或新建路径（不许"某组件"级泛指）；
2. **接口形状**：Rust/TS 函数、类型、IPC command 的字面签名（实现可照抄级）；
3. **数据变更**：migration/表/JSON 文件结构 + 旧数据兼容策略（serde default 等）；
4. **门禁联动**：capability/permissions 文件、host schema 注册、安全负例测试名；
5. **回归清单**：每任务至少一正一反，测试**字面名**写进文档（写不出名字=没设计过断言）；
6. **完成判据**：机器可判定语句（测试绿/冒烟步骤+预期可观测值），拒绝"体验良好"式判据。

现状核账：B9 已按本标准全文（10 文档 §3–§9 即样板）；B0 按本标准补齐见 §3 末六栏表；B1–B8 各批维持现状粒度（已有 文件:行+组件名+验收），**升级为六栏是该批首个代码提交前的先行动作**（与"方案先行"同轨，不额外造一轮文档运动）。调研/设计层文档（panels 细案、台账）不承担六栏义务，以本表链接为桥。

## 3. B0 宿主壳补齐

1. **MainWorkbench 注册表化**：`lazy` 声明 / `isXxx` 布尔 / 副标题三元 / 渲染三元四处合并为 `PANELS: Record<ModuleId, { panel: LazyExoticComponent; subtitle: string }>`；MODULES 表新增 `panelKey` 或 id 直用。收益：新模块缺壳从"静默兜底"变"类型系统报错"。
2. **ScreenshotPanel（新建 `src/modules/screenshot/ScreenshotPanel.tsx`）**：截图历史网格（`screenshot_history_list`，缩略图 + 时间 + OCR 文本预览 + 点击放大/再复制/再 OCR）；贴图管理卡（`screenshot_pins` 列表：聚焦/关闭/全部关闭）；发起截图按钮（复用 `overlayController.startOverlay("shot"|"ocr")`）；「计划中：录屏 v1.1（D-08）」占位 Badge。
3. **OcrPanel（新建 `src/modules/ocr/OcrPanel.tsx`）**：引擎状态卡（`ocr_engine_status`：注册引擎/可用语言/当前选择）；发起"截图取字"；本地图片文件识别入口（读文件 → `ocr_recognize({ request: OcrRequest })`，含语言下拉——兑现 B4 前的最小语言面）；结果文本区 + 复制（`ocr_copy_text` 回写窗口语义复用）。
4. **设置中心修复**：`MainWorkbench.tsx:201` 硬编码 `moduleId="clipboard"` → `moduleId={active}`，各模块 schema 键在 `host_config_schema` 已按模块存在（校验：给 screenshot/proxy 打开设置渲染出各自表单的正例测试）。
5. **占位 Badge 机制（D-29 决策 4）**：新组件 `DeferredBadge({ label, decisionRef })`——outline Badge "计划中（v1.1）" + Tooltip 指向 DECISIONS 条目；首批挂点：录屏(D-08)、Paddle/翻译(D-08)、每显示器覆盖(D-23)、T4→已排期则不挂、网盘(B6 排期前)。
6. **回归**：vitest 新断言——遍历 `MODULES` 每个 id 在 `PANELS` 注册表必有键（或显式 `placeholderOf`），负例：删一键即红；`inlineStyleCount` 同款纯函数风格 + MainWorkbench 无 "模块界面待实现" 字面量可达路径（注册表兜底分支删除）。

验收：实启冒烟走查截图/OCR 面板实数据渲染、任一模块设置面板跟随、托盘触发 quickpanel/overlay 无回归；门禁全组。

### 3.1 B0 六栏任务书（五审按 §2.5 标准补齐；上列 1–6 为设计叙述，本表为动工工单）

| 任务 | 目标锚点 | 接口形状（字面） | 数据变更 | 门禁联动 | 回归（正反例字面名） | 完成判据 |
|------|----------|------------------|----------|----------|----------------------|----------|
| T-B0-1 注册表化 | `src/layout/MainWorkbench.tsx`（四处 lazy/isXxx/副标题/渲染三元 + :201） | `export const PANELS: Record<ModuleId, { panel: LazyExoticComponent<ComponentType>; subtitle: string }>` | 无 | 无新命令 | `panelsRegistry_coversEveryModuleId`（正：MODULES 每 id 有键或 placeholderOf）；`panelsRegistry_missingKey_selfCheckTurnsRed`（负：构造缺键表必红）；`mainWorkbench_noFallbackLiteral`（"模块界面待实现"字面量 src 内零命中） | tsc 报错即兜底消失（类型系统判据）+ vitest 三名绿 |
| T-B0-2 ScreenshotPanel | 新建 `src/modules/screenshot/ScreenshotPanel.tsx` + PANELS 挂行 | `invoke<ScreenshotHistoryItem[]>('screenshot_history_list')`；`invoke('screenshot_pins')`；`overlayController.startOverlay(mode:'shot'|'ocr')`（既有签名复用） | 无（DTO 以后端 wire.rs 现形为准，前端 types 镜像） | 命令既有，无权限变更 | `screenshotPanel_rendersHistoryRows_fromMockedInvoke`（正）；`screenshotPanel_emptyVsNoResult_twoCopy`（负：空库与搜索无果文案不同，00§4-4） | 实启冒烟：历史网格出真缩略图+点击再复制；vitest 二名绿 |
| T-B0-3 OcrPanel | 新建 `src/modules/ocr/OcrPanel.tsx` + PANELS 挂行 | `invoke<OcrStatusDto>('ocr_engine_status')`；`invoke('ocr_recognize', { request: OcrRequest })`（commands/ocr.rs:10 实签：request 对象非裸 b64，含 source_task_id 回填语义）；复制走 `ocr_copy_text(text)` | 无 | 同 | `ocrPanel_engineCard_fieldsFromStatus`（正）；`ocrPanel_recognizeError_showsWhyEmpty`（负：引擎缺失时 EmptyState 说明原因+首动作） | 冒烟：选图识别出文本可复制；vitest 二名绿 |
| T-B0-4 设置中心跟随 | `MainWorkbench.tsx:201` | `moduleId={activeModuleId}`（prop 传递，非硬编码字面量） | 无 | 各模块 schema 已在 host `register_schema`（config.rs:111） | `settingsCenter_followsActiveModule_screenshotAndProxy`（正：两模块各渲出自身表单字段）；`settingsCenter_moduleIdProp_noClipboardLiteral`（负例式静态断言） | 冒烟：切模块开设置见不同表单 |
| T-B0-5 DeferredBadge | 新建 `src/components/DeferredBadge.tsx` | `export function DeferredBadge(props: { label: string; decisionRef: string }): JSX.Element`（outline Badge+Tooltip 指 DECISIONS） | 无 | 无 | `deferredBadge_tooltipCarriesDecisionRef`（正）；`deferredBadge_neverRendersAsEnabledButton`（负：不得出现可点击主按钮形态） | 首批挂点（录屏 D-08/Paddle D-08/每显示器 D-23/网盘 B6）渲染可见 |
| T-B0-6 SUBNAV 通用化 | `src/layout/SubNav.tsx`（CLIP_GROUPS 硬编码处） | `SUBNAV: Record<ModuleId, SubNavSection[]>`，`SubNavSection = { group: string; items: { id; label; icon; badgeKey? }[] }`（panels/README 总则字面） | 无 | 无 | `subnavRegistry_coversAllModules`（正）；`subnav_clipboardGroupsMigrated_countsPreserved`（回归：剪切板现有分组/计数一项不丢） | 剪切板行为与迁移前逐项一致（快照断言） |

实施提交切法：T-B0-1+6 同提交（注册表原子面）、2/3 各一、4+5 可并；每提交门禁全组。

## 4. B1 孤儿命令接线全表（后端已有 → UI 补入口）

| 面板 | 命令 → 入口 |
|------|-------------|
| clipboard | `clipboard_clear(keepPinned)` → 工具栏「清空」（ConfirmDialog + 保留置顶开关，D-18 基线）；`clipboard_get`/`clipboard_get_image` → 行详情抽屉（全文/大图 + 单独复制）；行菜单补「仅复制」（不粘贴）|
| vault | `vault_change_master_password` → 解锁态设置区表单（旧密/新密/确认，红线负例：错旧密拒）；`vault_folder_rename` → 文件夹行内改名；`vaultEntries` search 参数接搜索框（后端已过滤，前端去 filter）；`vault_entry_get` → 详情抽屉懒加载 |
| file | `file_search` → 工具栏搜索框（degraded 标记如实显示"索引降级：逐目录遍历"）；`file_preview` → 右侧预览面板（文本/图片/缩略图，蓝本 §3.4 预览兑现）；`file_rename_plan/apply` + `file_rename_entry` → 「批量重命名」对话框（DSL `{name}{n:03}` + 正则 + 预览表 + 应用/冲突列）；`file_drivers` → 侧栏驱动树（本地盘 → 未来网盘挂载点）；压缩/解压按钮（enqueue compress/extract，OP_KIND_LABEL 已备） |
| desktop | `desktop_notes_due` → 到期提醒条数据源改主动拉取 + 刷新；启动器区块补「重建索引」按钮（`desktop_launcher_status` 已有，触发命令勘查 B 方案：若无则新增 `desktop_launcher_reindex`，按孤儿表走 D-29 B1 例外登记） |
| kvm | `kvm_send_clip` → 已连接设备行「推送剪贴板」按钮；`kvm_send_file` → 「推送文件」（路径输入+确认，大小提示）；`kvm_session_list` → 会话表渲染（现在只判空不展示） |
| editor | `editor_save_as` → 「另存为」+ 原生文件对话框；`editor_update` → 远端置内容通路（配合 notes 跳转打开）；编码/EOL Badge 改可点下拉（后端能力勘查，若仅检测则登记收窄）；草稿恢复：启动时扫 `.nforge-autosave` 弹提示 |
| term | `term_ssh_known_hosts`/`term_ssh_forget_host` → 「已知主机」对话框（指纹列表 + 删除，红线：删除确认） |
| sys | `sys_clean_targets` → 清理页目标管理视图（内置清单只读 + 勾选态持久化展示）；`winops_catalog` → 「调整目录」浏览页（43 项按 family 分组 + 生效方式说明，替代只能 scan 后看）；`host_module_restart` → 模块状态面板重启按钮（D-19 后端在，UI 缺） |
| notes | 搜索框改后端查询（notes-core FTS 通路勘查后接）；`notes_sync` 加手动「同步」按钮 |
| screenshot/ocr | （并入 B0 面板本体） |
| automation | （动作表单补 `ipc_command` 项——属 B7 规则编辑器深化，B1 不动） |

每行一个提交或小批；每条 UI 路径带 vitest 渲染/交互测或实启冒烟记录；死按钮负例：新按钮必须真实 invoke（禁止只写日志）。

## 5. B2 代理多内核（蓝本 §3.2：v2rayN 多核管理 + FlowZ 换核）

架构（先解构再扩展，顺序不可倒）：

1. **配置 IR（新 `proxy-core/src/ir.rs`）**：协议无关中间表示——`Inbound{system_proxy_mixed, tun}`、`Outbound{Node|Direct|Block|Selector|Urltest}`、`Rule{type,pattern,target}`、`Dns{...}`；`config.rs` 重构为 `singbox::render(ir)`，现有节点/模式/直连规则语义映射为 IR 构造（golden-file 对照：重构前后 sing-box JSON 逐字节等价，负例：IR 非法组合拒）。
2. **内核注册表 + CoreProvider 升格**：`KernelDriver` 扩展 `config_render(&self, ir)->String`、`supported_protocols`、`validate_config`；`service.rs:463` 硬编码改注册表查找 + `proxy_core.kernel` 配置字段（默认 sing-box，兼容现装）；`spawn_kernel` 提为 pub。
3. **XrayDriver**（sidecar 下载 `Xray-core` release，二进制 `xray run -c config.json`，格式: log/inbounds/outbounds/routing——IR 第二生成器）；**MihomoDriver**（clash 配置 YAML 生成，`mihomo -d dir`，mixed-port/tun/proxy-groups/rules 映射；GeoDB 随 sidecar 下载说明）。三内核共存、状态互斥（同一时刻仅一核运行）、换核 = 停旧 → 用新方言重渲染 → 启新 → 系统代理/TUN 重挂（现有 sysproxy 生命周期复用）。
4. **订阅与协议扩展**（`sub.rs`）：`NodeKind` 扩 Hysteria2/TUIC-5/WireGuard/ShadowsocksR（URI 参数解析逐个带畸形负例）；**Clash YAML 订阅解析器**（proxies/proxy-groups 白名单子集，拒绝任意 JS/插件字段——防订阅投毒，红线批）；ss:// plugin 参数解析。
5. **规则体系**：geoip/geosite 支持——sing-box/xray 走内嵌 geodata 下载（sidecar 通道）+ `rule-set`/`geoip` 引用；mihomo 走 `GEOSITE,xxx` 规则；UI「分流规则」页：直连/代理/拒绝三列规则编辑（type: domain/suffix/keyword/ip-cidr）+ 内置"大陆常用直连"预设（蓝本预设模板思想）。模式从 off/global/rule 扩 `rule` 真分流（现状 rule 仅 ip_is_private+用户后缀）。
6. **测速与选节点**：`delay_test` 改 HTTP 204 真实探测（timeout/失败率），保留 TCP RTT 为回退；新命令 `proxy_node_select`（手动选定出口，selector 组固定 default）/`proxy_node_auto`（urltest 间隔与策略）；节点表行操作（选定/测单个/测全部）；内核启停/重启按钮（`proxy_kernel_restart`）。
7. **Pre-Socks 双核协作（蓝本 §3.2 TUN 段）**：**登记为二期**（mihomo/xray 齐后另立小节），避免 B2 膨胀为 XL×2；D-29 决策 2 已注明 B2 范围。
8. UI：ProxyPanel 顶部「内核」选择卡（版本/安装/切换，复用 kernel_install 通路按 core 参数化）+ 分流规则页 + 节点操作列 + 日志级别过滤。
9. 权限面：全部新命令同步 `permissions/` + main capability（D-28 机制自动兜底）。
10. **与 B9 合流（五审新增，防遗忘钩子）**：xray/mihomo 内核二进制与其 geo/rule-set 资产上线即注册进 `artifact-core` 注册表（[10 文档](10-artifact-auto-update.md) §3 占位行），禁止另写内核特例更新器；mihomo 系优先接通**内核原生** `geo-auto-update/geo-update-interval` 与 rule-provider `interval`（IR 的 rule/geo 段带该字段，由内核自刷），应用轨只兜底非内核托管者（sing-box 的 geoip.db/geosite.db、内核二进制本身）。

验收：三内核配置生成 golden-file；换核 e2e（测试双驱动断言生命周期）；订阅解析含 5 类畸形负例； Clash YAML 投毒负例（JS 字段拒）；系统代理崩溃恢复回归不破（D-06 语义）。

## 6. B6 文件与网盘多协议（蓝本 §3.5/3.6：OpenList 驱动注册表 + AeroFTP/Filestash）

1. **`TransferProtocol` 抽象（file-core 新 `transfer.rs`）**：蓝本 §3.6 方法集原语化——connect/disconnect/list/upload/download/delete/rename + `ConnectionConfig{protocol,host,port,auth}`；与既有 `host_core::storage::StorageDriver` 的关系：**Transfer 为"会话"、StorageDriver 为"挂载后驱动"**，每个协议实现一个适配器注册进 `DriverRegistry`（扩展位 driver.rs:92 已留）→ FilePanel 驱动树（§4）透明浏览远端根，复用队列/冲突/断点引擎。
2. **协议实现（依赖选型即 D-29 决策 6 一部分，全部纯 Rust 避免 sidecar 依赖）**：FTP/FTPS=`suppaftp`+native-tls；SFTP=`russh-sftp`（term-core 已有 russh 栈，凭据路径独立）；WebDAV=reqwest PROPFIND/PUT/MKCOL/COPY/MOVE 最小客户端；HTTP(S) 下载并入现有队列（进度/暂停/续传白捡，Range 断点）。**服务商预设模板**（蓝本 44 预设思想）：内置 JSON 预设表（常见 WebDAV/S3 兼容端点骨架），填凭据即用——数据文件非代码。
3. **凭据面（安全红线）**：远端口令存储走 vault-core 信封（新 `file_credential` 端口 → vault 未解锁时提示），不落明文 JSON；known_hosts 复用 term TOFU 语义（SFTP）。负例必含：凭据导出不含密码字段、hostkey 变更拒连。
4. **rclone 驱动**：作为 DriverRegistry 第 5 注册位——依赖 D-08 sidecar 分发通道（下载 rclone portable zip，校验 SHA256，同 proxy kernel 模式）；`RcloneMountDriver` 经 rclone `serve webdav` 本机回环？——**开工前在本节先落子方案裁定（native 实现 vs rclone 二选一按驱动族）**；网盘（阿里/腾讯/OneDrive 等 OpenList 驱动族）v1 只经 rclone 后端覆盖，不逐厂适配（记录）。
5. UI：FilePanel「新建连接」对话框（协议下拉/主机/端口/用户/口令/匿名/预设快选）→ 连接列表 → 双击进远端浏览（复用目录表）；传输任务并入操作队列视图；`file_drivers` 返回驱动树后侧栏真实化。

验收：每协议测试双 fake server（tiny-http 起 WebDAV 桩、suppaftp 测试本地 ftp? 若不可行则录制回放桩）+ 断点/冲突/权限位负例；凭据红线批；能力 ACL 同步。

## 7. B7 终端/键鼠/系统/自动化/桌面/编辑/笔记深化（蓝本 §3.7/3.8/3.11/3.12）

**term（§3.11 NyaTerm）**：①端口转发 -L/-R/-D（`direct-tcpip`/`tcpip-forward`/本地 SOCKS5 server——§5 推翻项）+ 转发管理表 UI（本地监听端口冲突检测）；②ProxyJump 链式连接（config 字段 jump: [host]，逐跳 TOFU）；③非交互 exec（命令面板跑远端一次性命令回显）；④SFTP 补 delete/mkdir/rename/权限位 chmod；⑤known_hosts UI（B1 已列）；⑥SSH 别名/配置导入（解析 `~/.ssh/config` 子集 Host/HostName/User/Port/IdentityFile——只读导入不写回）。
**kvm（§3.7 Lan Mouse）**：①XButton1/2、水平滚轮投影进 `RawInput`（capture+inject 两端，`input.rs:189` 注释兑现）；②边缘扩 up/down 四缘（edge.rs 映射已泛化则仅 UI Select 扩）；③拖拽文件跨机（KVM 文件通道 transfer.rs 已有，接"文件被拖过边缘"为 v1.1 子方案，本批先做右键"推送到对端"）；④修饰键状态同步（边缘切换瞬间 CapsLock/NumLock 对齐）。
**sys（§3.12 NeoHtop + §3.8 UniGetUI）**：①进程页：top-N 排序列表（CPU/内存/磁盘 IO）、搜索、结束进程（确认 + 系统进程保护黑名单红线负例）；②托盘负载显示（tray set_title 通路 D-26 已有——"CPU x% · MEM y%"周期刷新，可配关闭）；③包管理：`PkgManager` 扩 `search(query)`（winget source search+show、scoop search、choco list）→ 在线搜索安装对话框；单包升级动作；choco 需管理员路径走提权 helper 说明；④winops needs_admin 失败的「以管理员重试」按钮（helper 通道已在）。**⑤三审扩充（用户直令 2026-09-19，蓝本无此专节）**：卸载与残留（双栈已装列表/批量静默卸载/置信度分级残留扫描，BCU 范式）、Windows 更新管控（档位三选/暂停延迟/驱动排除/隐藏 KB/WUA 路线/危险区彻底禁用+ExpertGate 门禁）、还原点/上下文菜单扩展/驱动导出/电源计划/关联导出导入/ AppX 取回/MFT treemap 工具箱——细案 `docs/panels/2026-09-19/07-sys.md` §8，调研台账 `90-research-system-ui.md` §1；动工前登记 D-31 五要素。
**automation**：①动作下拉补 `ipc_command`（后端 ActionDto 支持、表单缺）；②`then` 多动作数组编辑器（现编辑会静默丢动作——数据完整性 bug 级，优先）；③条件 and/or 一层嵌套 UI；④规则执行历史（新事件订阅落 ring buffer + 面板视图）与试运行 dry-run；⑤死信批量重放/清空。
**desktop**：①整理规则自定义（分类→目标夹映射编辑，tidy plan 后端参数勘查）；②启动器：「打开启动器」按钮 + 热键说明 + 重建索引（B1）；③随记 #标签过滤视图。
**editor**：①编码/EOL 真实切换（保存转码，后端能力勘查登记）；②PDF 拆分页数区间输入（现硬编码 `_pages`）；③会话持久化（重启恢复标签）。
**notes**：①编辑器换 Monaco（复用 editor 基元）+ `[[双链]]` 补全弹窗（后端 links 已有）；②标签过滤条 + 大纲面板；③画布节点就地编辑 + 删连线；④复习统计卡；⑤搜索走后端（B1）。

每子模块一子批、独立提交带回归；红线子批：sys 结束进程、term 转发（端口暴露）、kvm 注入。

## 8. B3 剪贴板深化（蓝本 §3.1 Ortu 核心功能清单）

①**粘贴堆栈**：多选入栈 → 全局热键顺序粘贴到焦点应用（后端新命令 `clipboard_stack_push/pop/clear` + win-integration 逐条粘贴通路，复用 paste 实现）；面板堆栈视图。②**智能自动分组**：规则分类器（URL/代码/JSON/Shell/密钥/邮箱 正则族 + 置信度）在 classify 阶段落 `suggested_group`，UI 一键采纳（**不自动改用户分组**，保守同 D-05）。③**敏感掩码与按需揭示**：加密条目列表内默认掩码预览，点击揭示（解密口已有，UI 层）。④**暂停捕获开关**持久化（config_store 键 + 重启恢复 + 托盘菜单项联动 D-26 通路）。⑤**导出/导入**（加密库 JSON 导出含密码二次确认；导入合并去重）。⑥RTF 捕获与富文本粘贴（Win32 CF_RTF 投影，勘查 win-integration 后定深浅）。⑦搜索语法 `group:<name>` / `type:` 过滤（FTS 查询前缀解析）。

## 9. B4 截图/OCR 深化（蓝本 §3.9 ShareX 流水线 + §3.10 Umi-OCR）

**截图**：①标注图层系统（z-order 列表面板、上移/锁定/选择穿透——AnnotationDto 加 `layer` 字段向后兼容）；②工具补：直线、高亮/荧光笔、模糊（高斯 vs 马赛克区分）、图形填充开关、取色器（蓝本像素级放大镜+颜色拾取）、自定义颜色盘 + 透明度、text 内联编辑（弃 `window.prompt`）；③捕获模式补：窗口捕获（PrintWindow）、滚动截图（分段拼接，失败降级提示）；④**美化**：圆角/阴影/padding/背景渐变导出（canvas 后处理，OpenSnap 卖点）；⑤导出格式 JPEG/WebP 选择 + 保存路径模板 UI（types.rs `{ts}` 已支持）；⑥**任务流水线**：截图完成后动作链可配置（复制/保存/OCR/钉图/上传 组合预设，现 actions 数组已具雏形→提为配置）；⑦**UploadProvider trait**（蓝本 IUploaderProvider）：HTTP 表单上传 + WebDAV 目标（复用 B6 Transfer 会话）+ 复制直链，Provider 注册表 + UI 选择；不内置图床账号。
**OCR**：①**Tesseract 引擎**（`tesseract-rs`/FFI，本地安装探测，EngineRegistry push 即得，D-09 抽象兑现）：引擎切换 UI + 按语言择优；②语言选择真实化（OverlayShot `langs:[]` 恒空 → 设置页语言多选 + UI 语言 BCP47 列表来自 `ocr_engine_status`）；③**图片文件批量识别**（OcrPanel 拖入多文件 → 逐张识别 → 段落合并导出 txt/md，Umi-OCR 管线；PDF 走 editor 拆页后 OCR 串联）；④结果结构补 `confidence`/块位置（OcrLine 已有位置，DTO 透出）供"按块复制"；⑤**翻译槽**：`OcrResultDto.translate` 可选字段 + Provider trait 空实现（D-08 维持：不接在线翻译服务，接口留位，避免蓝本 §3.10 联动断裂）；⑥PaddleOCR：模型分发子方案另立 `09b-paddle-sidecar.md` 开工前写（体积/下载通道/崩溃恢复三条 D-08 验收欠账）。

## 10. B5 sync 深化（现状判定：后端缺接口，非纯 UI）

①**per-peer 状态查询**：`sync_status` 扩 `{peer, addr, last_sync_ms, cursor_local, cursor_peer, pending_ops, last_error}`（oplog 游标已 per-device，透出即可）+ 冲突历史落盘表（现只发一次性事件——追加 SQLite conflict 表）。②**自动同步**：变更后 debounce 触发（订阅 notes/clipboard oplog 写点事件）+ 可选定时（config 键）；面板开关真实化（"同步范围"纯文字 → 数据集 Checkbox：笔记先通、剪贴板二期）。③**发现复用**：KVM UDP 心跳已带 `tcp_port`——sync 对端列表从 kvm paired + discovered 合成，`sync_now` 免手填地址（保留高级手输）。④**entity 白名单泛化**（`module.rs:39`）：注册表化 `{entity → (读快照 fn, 应用 fn)}`，为剪贴板入流留位（vault **永不入流**红线维持，lib.rs:6）。⑤面板补：传输日志、暂停/恢复、每 peer「立即同步」、冲突列表视图（LWW 结果展示 + 被覆盖快照查看——3-way 合并仍 §5 不做，只读历史不违背）。⑥`sync.conflict` 事件面板监听（现未监听）。

## 11. B8 待裁决（只出方案，不写代码）

1. **办公助手**（蓝本第八节 OpenLoaf/AionUi 系）：立项 = 新模块 editor+notes+AI 后端（本地 Agent 运行时或远程 API 凭据）三选一路线评估；给出一页方案后等用户裁决，**默认不做**。
2. **i18n 复审**：§5 仍裁不做；安装器语言割裂（English 装 UI 中文）挂此节待议。
3. **Pre-Socks 双核 TUN 协作**、**KVM 拖拽传文件**：B2/B7 尾大不装，各自子方案节（开工前补写）。

## 12. 验收惯例与状态跟踪

- 门禁全组每提交必跑（fmt/clippy -D warnings/test --workspace/bench --no-run/tsc/eslint/vitest/build），pipefail 不掩码。
- 每提交回归测试；安全红线批（B6 凭据、B7 进程/转发/注入、B1 known_hosts 删除）必含负例。
- 新命令一律同步 `src-tauri/permissions/*.toml` + main capability（security_config.rs 构建期兜底）。
- 批次状态表：

| 批次 | 状态 | 证据 |
|------|------|------|
| B0 | 未开工 | — |
| B1 | 未开工 | — |
| B2 | 未开工 | — |
| B3–B7 | 未开工 | — |
| B8 | 方案待裁决 | — |
| B9 | 未开工（码级任务书已立于 [10-artifact-auto-update.md](10-artifact-auto-update.md)，D-32 待登记） | 五审台账 92 |
