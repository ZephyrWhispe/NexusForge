# 面板详细设计档 · 2026-09-19（D-29 附属）

> 地位：本文件夹是 `docs/impl/09-blueprint-alignment.md`（D-29 总纲）的**逐面板实施细案**。总纲管批次与依赖，这里管每个面板"有哪些问题、要长什么样、对标谁、抄什么、超什么"。开工任何面板前，以本档对应文件为准；本档修订先于代码。
> 用户裁定（2026-09-19）：①对标参考软件**全部功能**，不是主要功能；②**UI 不得把所有内容塞同一面板**，按内容分类拆子面板；③吸收蓝本之外其他优秀软件的好点子；④要有"常人未思"的拓展设计。

## 0. 信息架构总则（所有面板共同遵守）

### 0.1 子面板机制：SubNav 通用化

现状 `src/layout/SubNav.tsx` 硬编码剪切板分组（190px 左栏，仅 CLIP_GROUPS）。提升为全应用机制：

```
SUBNAV: Record<ModuleId, SubNavSection[]>
SubNavSection = { id, name, icon?, kind: "view"|"filter"|"group", count?: 动态计数 }
```

- 每个模块工作区 = 左二级导航（子面板）+ 右内容区；一次只显示一个子面板，**禁止无限纵向堆长表单**（现状 ProxyPanel/FilePanel 的病根：一切区块竖排一屏滚）。
- 子面板内允许 Tabs/折叠，但顶层分类必须互斥清晰。
- 注册表化与 B0 的 `PANELS` 表合并实施（MainWorkbench 一处真相）。

### 0.2 五类子面板规范（分类学）

每个模块的子面板必须落入以下五类之一，保证跨模块心智一致：

| 类型 | 职责 | 例 |
|------|------|-----|
| 概览 | 状态、开关、关键数字、健康自检 | 代理"运行总览"、同步"概览" |
| 清单 | 可搜索/过滤/批量操作的实体表 | 节点、条目、文件、规则、设备 |
| 编辑 | 单实体深度操作（对话框或占区） | 条目编辑、标注器、规则编辑器 |
| 历史/日志 | 时间线视图 + 导出 | 内核日志、审计、冲突、执行历史 |
| 设置 | 本模块全部可调参数（从功能区迁出） | 自动锁定时长、端口、语言、主题相关 |

**设置归位纪律**：功能区内只留状态与开关，阈值/路径/策略一律进"设置"子面板或宿主设置中心（二者同一 schema，不得两处漂移——B0 修复 `moduleId="clipboard"` 硬编码后，宿主设置中心即各模块 schema 之家）。

### 0.3 通用组件（随 B0 落地）

- `DeferredBadge`：已推迟能力（D-08/D-23/§5）在面板挂"计划中（v1.1）"+tooltip 指回 DECISIONS；
- `DataToolbar`：清单类子面板统一工具条（搜索框、过滤器、批量选择、分页/虚拟滚动）；
- `EmptyState/ErrorState`：沿用 D-18 基线，错误必附"重试/查看日志"；
- 危险操作统一 ConfirmDialog（D-18），不可逆操作要求输入确认词。

### 0.4 每档模板与验收惯例

每份面板档含：§1 现状问题（带 file:line）→ §2 子面板信息架构 → §3 对标功能矩阵（全部功能逐条）→ §4 其他软件借鉴 → §5 拓展设计（冷门但高价值）→ §6 验收点 → **§7 排版方案与二审补充（2026-09-19 复审增）**：本模块各子面板的布局骨架与元素摆位（通用纪律引用 [00-ui-layout-spec.md](00-ui-layout-spec.md) 条款号，不重复），以及复审发现的**配套功能缺口**（"主功能↔配合件"链，如代理之分流规则）与锦上添花设置——§7 增补条目归口到 §2 对应子面板与 §3 矩阵，动工以合并后清单为准。
批次映射不变（总纲 §2）；门禁全组 + 每提交回归 + 红线负例照常。

## 1. 目录

| 文件 | 模块 | 批次 |
|------|------|------|
| 00-ui-layout-spec.md | 全应用 UI 布局与控件规范总纲（各档 §7 共用底座） | B0 |
| 01-clipboard.md | 剪切板 | B1+B3 |
| 02-proxy.md | 代理与 VPN | B2 |
| 03-vault.md | 密码库 | B1+B7(自动-type 类拓展另批) |
| 04-file-netdrive.md | 文件·网盘·FTP/WebDAV/HTTP | B1+B6 |
| 05-desktop.md | 桌面效率 | B1+B7 |
| 06-kvm.md | 键鼠共享 | B1+B7 |
| 07-sys.md | 系统管理·软件·监控 | B1+B7 |
| 08-term.md | 终端运维 | B1+B7 |
| 09-screenshot.md | 截图与贴图 | B0+B4 |
| 10-ocr.md | OCR | B0+B4 |
| 11-editor.md | 文本与 PDF | B1+B7 |
| 12-notes.md | 笔记与知识 | B1+B7 |
| 13-automation.md | 自动化 | B7 |
| 14-sync.md | 同步 | B5 |
| 15-host-shell.md | 宿主壳（导航/命令面板/设置中心/托盘/快捷键总览） | B0+B8 提案 |
| 90-research-system-ui.md | 三审调研台账：系统管理功能面（卸载/更新管控/工具箱）+ UI 设计案例（结论已归口 07-sys §8 与 00-spec §9） | 证据档 |
| 91-research-panels-scaling.md | 四审调研台账：除系统面板外 14 档功能增量 + UI 缩放/自适应重排证据（结论已归口各面板 §8 与 00-spec §10） | 证据档 |
| 92-research-final-audit-updates.md | 五审终审台账：个人非商用借鉴裁定 + 内核/geo/订阅自动更新借鉴表 + 代码级任务书标准 + 14 条注意事项（结论归口 [docs/impl/10-artifact-auto-update.md](../../impl/10-artifact-auto-update.md) 与 09 §2.5） | 证据档 |

UI 预览样张（静态 HTML，浏览器直开，审核用非代码）：[preview/index.html](preview/index.html)——19 屏覆盖全部功能面板：主工作台骨架·剪切板、系统·软件与卸载、系统·更新管控、设置页规范样例、代理·节点与分流规则、密码库、文件与网盘、桌面启动器、键鼠共享、终端、截图历史、OCR 批量、PDF 流水线、笔记、自动化、同步、宿主设置中心·快捷键，S18 三宽度（1280/960/620）缩放重排演示，S19 宿主·更新中心（B9 构件自动更新 UI 面）；按 00-spec（含 §9、§10 增补）1:1 摆放，断点态由各窗口的 CSS container query 独立呈现。

## 2. 调研来源（本档借鉴依据，2026-09-19 检索）

- 截图贴图类：Snipaste（贴图 F3 剪贴板钉图、贴图组、鼠标滚轮缩放/透明度/灰度）、PixPin（滚动长截图、OCR、贴图钉 GIF/文字/文件）、ShareX（任务流/上传目标/OpenGraph）、Ksnip；
- 代理类：v2rayN（多内核切换、节点分组/排序/测试、订阅流量到期位）、Clash Verge Rev（内核管理、TUN、规则可视化、延迟历史）、FlowZ/AureStream；
- 终端类：WindTerm（会话树/分组、命令片段、会话录制、SFTP 双栏、端口转发管理器）、MobaXterm（多会话广播、X server）、Tabby（插件市场、配色）；
- 剪贴板类：CopyQ（标签页、脚本命令、粘贴队列）、Ditto（批量粘贴、搜索秒贴）、ClipAngel/ClipDiary（按应用规则、历史保护）、Espanso（文本片段扩展）、uTools/Quicker（魔法命令/动作库/组合命令）；
- 系统类：UniGetUI（多包源 GUI）、WinUtil（清理/优化策略）、NeoHtop（进程树）、TreeSize/SpaceSniffer（磁盘 treemap）、PowerToys（FancyZones/PowerRename/Advanced Paste/Find My Mouse 等单点好点子）；
- 文件类：Files（标签/双栏/详情窗格/文件色标）、Oxyde_FM（多面板）、Directory Opus（过滤器）、voidtools Everything（秒搜心智）、AeroFTP/Filestash（多协议）、Cloudreve/Alist/OpenList（存储驱动与分享）；
- 同步类：Syncthing（设备/文件夹模型、版本历史、忽略模式、带宽调度）、LocalSend（局域网直传、PIN 配对）；
- OCR 类：Umi-OCR（离线批量、排版合并、二维码、翻译接口位）、PaddleOCR、Tesseract/tessdata、eSearch。
- 仓库侧：两次只读勘查（12/12 面板 + 177 命令 + 7 crate），证据行号散见各档 §1。
