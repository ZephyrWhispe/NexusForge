# 90 专项调研台账：系统管理功能面 + UI 设计案例（2026-09-19 三审）

> 地位：调研**台账**（证据档），不是实施细案——结论已归口到 `07-sys.md` §8、`00-ui-layout-spec.md` §9、`docs/impl/09` B7 段；本档保留完整功能清单与来源供追溯。用户裁定原话："系统相关的像什么卸载然后禁止更新之类的功能也要完善 也要添加进去 还有没有提到的一些系统相关的功能也要添加进这个软件。查找一些比较优秀的UI设计案例参考学习。"

## 1. 系统管理软件功能调研

### 1.1 逐软件功能块（重叠度：已有/部分/全新，对照本仓 sys 现状）

**WinUtil（ChrisTitusTech，PowerShell/WPF）——"更新档位化"是最值得抄的形态**
- Windows Update 三档 Profile：Recommended（延迟特性 365 天/质量 4 天、`ExcludeWUDriversInQualityUpdate=1`、登录不自动重启）/ Windows Default（删策略+恢复服务与计划任务）/ Advanced disable（`NoAutoUpdate=1`+`AUOptions=1`+停并禁用 BITS/wuauserv/UsoSvc+清 SoftwareDistribution+禁用含 WaaSMedic 的 6 组更新计划任务）→ **全新**；
- 修复 Windows Update 12 步重置（停服务/删 BITS 作业/重命名 DataStore、CatRoot2/重注册 DLL/清 WSUS/reset winsock/触发扫描，Aggressive 分级）→ **全新**；
- 细粒度开关委托外链 O&O ShutUp10++（"专业工具出口"边界做法可借鉴）；
- 67 项 tweaks 四组（Essential/Customize/Advanced-CAUTION/Performance Plans）+ `Get Installed Tweaks` 状态回读 + Undo Selected + Restore Defaults + 三档 preset.json → **部分**（我们有 43 项目录+回滚，缺状态回读/套餐/恢复默认）；
- AppX 双向：按清单卸载内置应用 + 重新安装/取回（provisioned 移除）→ **全新**；
- 运行时创建还原点、Disk Cleanup+删旧更新、释放保留存储、Ultimate Performance 电源计划、可选功能、驱动备份、默认应用、配置导入导出、诊断报告 → **全新**。

**BleachBit——安全网教科书**
- Preview→Delete 两段式、Confirm before delete、**Expert mode 默认过滤高危清理项** → 部分/全新（cmd_preview 同源，缺高危分级门禁）；
- CleanerML 声明式清理器（XML：删/截断/粉碎、注册表键值、glob/正则、OS 判定）+ winapp2.ini 2400+ 规则导入 → **部分**（缺"声明式规则库+可校验 schema"）。

**Bulk Crap Uninstaller（BCU）——卸载专项最完整**
- 10 类应用源统一列表（常规注册表、隐藏/受保护、卸载器损坏、便携应用扫描、Chocolatey、Steam、Windows 组件、Store/UWP、Windows Update 本体、网络）→ **全新**；
- 批量/静默卸载、卸载器冲突检测与并发、防重启拦截、崩溃挂起卸载器"跳过/立即运行/终止"、MsiExec 按 ProductCode、手动卸载（列文件+注册表键勾选）→ **全新**；
- 残留策略：卸载后扫描 + **Confidence 置信度分级（默认只勾 Good 以上）**、注册表删除前询问备份、文件走回收站、空目录/半卸载残留、孤儿应用检测 → **全新**（§7.2-① 卸载残留扫描的落地范式）；
- 启动项管理、treemap 大小视图、`.bcul` 卸载清单、XML 脚本规则、**模拟卸载（Simulate）**、运行前创建还原点 → 全新。

**HiBit Uninstaller + Startup Manager（免费）/ Revo / Geek**
- HiBit：完全/强制/批量卸载、**安装过程前后快照**、注册表修复、无效快捷方式清理、空文件夹查找、粉碎、还原点管理、服务与启动管理；Startup Manager 含**上下文菜单管理**与"谁拖慢开机"量化 → 全新/部分；
- Revo：Safe/Moderate/Extreme/Deep 四档残留扫描、强制卸载、安装快照回退；Geek：深扫残留、Force Removal、**最近安装/变更行高亮** → 全新（分档扫描+快照是差异化点）。

**Windows 更新管控族**
- WUMT（Windows Update MiniTool）：WUA API 搜索→列表→勾选下载/安装、**隐藏指定 KB**、删除/回滚已装更新、更新历史、直取更新文件、自动更新模式完全手动 → **全新**（证明"用 WUA API 而非纯注册表"可行）；
- WuMgr（DavidXanatos，开源，WUMT 的 .NET 重写）：同上+官方策略/服务方式管理；
- Windows Update Blocker（sordum）：一键禁/启服务、Wub.ini 屏蔽至多 25 个服务（含 WaaSMedicSvc）、**Protect service settings 防被回改**、CLI；官方承认副作用（商店、语音输入失效）→ 全新但**极高危**；
- InControl：锁定 `TargetReleaseVersion/TargetReleaseVersionInfo` 防强推到新周年版本 → 全新（低风险、可解释）；
- O&O ShutUp10++：数百开关含更新类别，作"专业出口"参照系；
- Manage Updates（Carl Schou）与 LockUpdateAsAdmin：**未取到官方一手资料，不采信**，登记待复核。
- Microsoft 官方策略面（waas-configure-wufb）：暂停最长 35 天、质量延迟 0–35、特性延迟 0–365、`NoAutoUpdate`/`AUOptions` 取值域 → 数值上下限的权威来源。

**Dism++ / ThisIsWin11 / Sophia Script（SophiApp）**
- Dism++：离线会话服务（不动运行系统=天然安全网思路）、清理更新备份/驱动缓存、Compact OS、驱动导出/导入、BCD 编辑 → 全新；
- ThisIsWin11：PumpedApp 去肿+恢复内置应用、Packages（winget 自定义安装清单）、TweakUI 复刻 → 部分/全新；
- Sophia/SophiApp：150+ 函数**每个 tweak 都有对应"恢复默认"函数**、gpedit 可见、计划任务式清理硬编码白名单（SoftwareDistribution/Temp/$WinREAgent 等）、**默认应用与文件关联导出/导入 JSON**、卸载 OneDrive、运行库安装 → 全新（关联导出、清理白名单两件套很实用）。

**清单/诊断工具族**
- WizTree：MFT 直读秒级 treemap（宣称比 WinDirStat 快 46×）、CSV 导出 → 部分（已规划 treemap，缺 MFT 快扫与导出）；
- **Autoruns**：23 类自启位置（Run/RunOnce、服务、驱动、编解码器、BHO、AppInit、映像劫持、Winsock、LSA、打印监视器、计划任务、WMI、Winlogon…）、Hide Signed Microsoft Entries、签名校验、Jump to Entry、离线映像扫描、autorunsc CSV → **全新**（我们启动项只规划三类源）；
- ShellExView/ShellMenuView：上下文菜单扩展启停 → 全新；WirelessKeyView：Wi-Fi 密钥导出（需管理员）→ 全新但**高敏感**；Winaero Tweaker：聚合式微调参照系。

### 1.2 汇总表（功能 → 来源 → 风险/提权 → 优先级）

| 功能 | 来源 | 风险/提权 | 优先级 |
|---|---|---|---|
| 注册表+UWP/MSIX 双栈已装列表与统一卸载 | BCU/HiBit/WinUtil | 低；HKLM 卸载串走提权 helper | 高 |
| 卸载后残留扫描（文件/注册表/计划任务/服务/上下文菜单/快捷方式）+置信度分级 | BCU/Revo/HiBit | 中；HKLM 删需提权 | 高 |
| 批量/静默卸载、冲突检测、失败跳过或终止 | BCU | 中 | 高 |
| 强制卸载（无卸载器/损坏） | BCU/HiBit/Geek | 高（不可回滚） | 中 |
| 安装快照 diff（前后对比） | Revo/HiBit | 中；后台监控成本 | 中 |
| 更新档位：默认/推荐/严格 三档互斥 | WinUtil | 中；HKLM 策略 | 高 |
| 暂停到日期/延迟特性(0-365)与质量(0-35)更新 | MS 政策+WinUtil | 低-中；策略键 | 高 |
| 驱动更新排除（ExcludeWUDriversInQualityUpdate+DriverSearching） | WinUtil | 中 | 高 |
| 交付优化 DODownloadMode/计量连接 | WinUtil/BCU | 低 | 高 |
| 隐藏/排除单个 KB | WUMT/WuMgr | 中；**WaaSMedic 会自愈**（文案诚实） | 中 |
| 锁定目标版本防强推（TargetReleaseVersion） | InControl | 低 | 中 |
| 更新组件重置修复（WU 12 步/DISM/SFC） | WinUtil | 高；提权 | 中 |
| 更新列表查看/回滚已装更新 | WUMT | 中；wusa 提权 | 中 |
| 彻底禁用更新服务（含 WaaSMedicSvc 屏蔽+防回改） | WUB/WinUtil | **极高**（商店/语音/安全更新失效） | 低（危险区+专家门禁才见） |
| 启动项深化（Autoruns 类多源+隐藏微软签名过滤） | Autoruns | 高 | 高 |
| 上下文菜单扩展管理 | ShellExView/HiBit | 高（误禁致 Explorer 异常） | 高 |
| 还原点：操作前自动创建+管理器 | WinUtil/HiBit/BCU/Sophia | 中；提权 | 高 |
| 磁盘 treemap MFT 快扫+导出 | WizTree | 低（只读；MFT 读需管理员，降级 walkdir） | 高（已在 §3-11 规划内升级） |
| 存储感知式清理+硬编码白名单+Preview | BleachBit/Sophia/WinUtil | 中 | 高 |
| 声明式清理规则库（CleanerML 式可校验） | BleachBit | 中（任意路径=红线，白名单不扩） | 中 |
| 内置 AppX 去肿与取回/重注册 | WinUtil/ThisIsWin11 | 中 | 中 |
| 驱动备份/导出（pnputil /export-driver） | Dism++/WinUtil | 低-中 | 中 |
| 默认应用与文件关联导出/导入 JSON | Sophia | 低 | 中 |
| 电源计划（含 Ultimate Performance，笔记本警示） | WinUtil/Sophia | 中 | 中 |
| 粉碎文件/擦除空闲空间 | BleachBit/HiBit | 高（不可恢复；SSD 上语义弱） | 低（登记边界） |
| Wi-Fi 密钥/产品密钥导出 | NirSoft/HiBit | **高敏感（凭据）** | 低（若做须 vault 纪律+一次性展示） |
| 配置导入导出+环境诊断报告 | WinUtil/SophiApp | 低 | 中 |

### 1.3 危险操作与业界安全网（对齐我们既有机制）

危险清单：**彻底禁用更新服务**（sordum 官方承认 Store/语音失效；WinUtil 源码注明"仅高级用户"）、**强制卸载/手动删注册表**（BCU 手册"除非别无他法不推荐"）、**删除上下文菜单/Shell 扩展**、**Bad 置信度残留**、**粉碎/擦除**、**隐藏 KB（WaaSMedic 自愈）**。

业界共同安全网，逐条对应我们已有部件：
1. **默认只列不改、预览先行**（BleachBit Preview、BCU Simulate）→ cmd_preview 纪律延伸；
2. **风险分级 + 默认勾选线**（BCU 只预选 Good 以上；WinUtil 危险项单列 CAUTION 组；BleachBit Expert mode 门禁默认过滤高危项）→ 新增 `ExpertGate`：危险区功能默认不可见，设置中显式开启并常驻黄色横幅；
3. **写前必备份**（BCU 删注册表前询问、文件进回收站；HiBit/Revo/WinUtil 应用前创建还原点；Sophia"每个 tweak 有恢复默认"）→ BackupStore/BAVR 既有 + 新增还原点通路；
4. **可撤销聚合**（WinUtil Get Installed Tweaks 回读 + Restore Defaults/Undo Selected）→ 我们 §5-④"一键撤销本工具全部改动"的同类已证实现；
5. **提权面收敛**（策略写成键集合便于精确删除而非乱改 ACL；Dism++ 离线会话不动运行系统）→ winops catalog id 前缀规范同族纪律。

### 1.4 来源（系统调研）

- [WinUtil docs](https://winutil.christitus.com/) · [tweaks.json](https://github.com/ChrisTitusTech/winutil/blob/main/config/tweaks.json) · [Invoke-WPFUpdatesdisable](https://github.com/ChrisTitusTech/winutil/blob/main/functions/public/Invoke-WPFUpdatesdisable.ps1) · [Invoke-WPFUpdatessecurity](https://github.com/ChrisTitusTech/winutil/blob/main/functions/public/Invoke-WPFUpdatessecurity.ps1) · [Invoke-WPFFixesUpdate](https://github.com/ChrisTitusTech/winutil/blob/main/functions/public/Invoke-WPFFixesUpdate.ps1) · [appx.json](https://github.com/ChrisTitusTech/winutil/blob/main/config/appx.json)
- [BleachBit 文档](https://docs.bleachbit.org/) · [General usage](https://docs.bleachbit.org/doc/general-usage/) · [Expert mode](https://docs.bleachbit.org/doc/expert-mode/) · [Shred/Wipe](https://docs.bleachbit.org/doc/shred-files-and-wipe-disks/) · [CleanerML](https://docs.bleachbit.org/cml/cleanerml/)
- [BCUninstaller 主页](https://www.bcuninstaller.com/) · [BCU 在线手册](https://htmlpreview.github.io/?https://github.com/Klocman/Bulk-Crap-Uninstaller/blob/master/doc/BCU_manual.html) · [GitHub](https://github.com/Klocman/Bulk-Crap-Uninstaller)
- [HiBit Uninstaller 功能表](https://www.hibitsoft.ir/Uninstaller.html) · [HiBit Startup Manager](https://www.hibitsoft.ir/StartupManager.html) · [Revo 特性综述(TechSpot)](https://www.techspot.com/downloads/5671-revo-uninstaller.html) · [Geek Uninstaller](https://geekuninstaller.com/)
- [WUMT(MajorGeeks)](https://www.majorgeeks.com/files/details/windows_update_minitool.html) · [WuMgr GitHub](https://github.com/DavidXanatos/wumgr) · [Windows Update Blocker](https://www.sordum.org/9470/windows-update-blocker-v1-8/) · [InControl](https://www.majorgeeks.com/files/details/incontrol.html)
- [MS：Configure Windows Update client policies](https://learn.microsoft.com/en-us/windows/deployment/update/waas-configure-wufb)
- [Dism++ 资源仓](https://github.com/Chuyu-Team/Dism-Multi-language) · [ThisIsWin11](https://github.com/builtbybel/ThisIsWin11) · [Sophia Script](https://github.com/farag2/Sophia-Script-for-Windows) · [SophiApp](https://github.com/Sophia-Community/SophiApp)
- [Autoruns(Sysinternals)](https://learn.microsoft.com/en-us/sysinternals/downloads/autoruns) · [ShellExView](https://www.nirsoft.net/utils/shexview.html) · [WirelessKeyView](https://www.nirsoft.net/utils/wireless_key.html) · [WizTree](https://www.diskanalyzer.com/) · [Winaero Tweaker](https://winaerotweaker.com/)

## 2. UI 设计案例调研

### 2.1 设置面板成熟范式

- **Fluent 官方 app settings 指南**：设置页**全屏单列、内容最大宽 1000–1100px**；分组 BodyStrong 标题；行卡 SettingsCard（Header+Description+控件右对齐定宽）＝我们的 SwitchSetting；子选项 SettingsExpander **嵌套 ≤1 层**；**每分组可见项 ≤4~5，低频折叠**；**修改即时生效、无"应用/确定"按钮**；禁用项必须 Description 解释原因；About 区置底一行；设置入口钉导航底部——与我们"设置=子面板末项"一致。
- **PowerToys (WinUI3)**：每模块页顶部独立"启用"总开关（已升 15 档矩阵行）；设置搜索 Ctrl+F 呼出、模糊匹配、建议 flyout、**结果 >5 条进"全部结果"整页**。
- **VS Code**：搜索带过滤芯片（`@feature`/常用预设）；**项级"恢复默认"在每项 gear 溢出菜单**；全局 JSON 双入口。
- **Windows Terminal**：独立设置窗、**自动保存无 Apply**；新版以 CheckBox 替代 Toggle 的争议——Fluent 官方立场仍是 ToggleSwitch 为二值首选，**我们维持 Switch，CheckBox 记为已评估不采纳**。
- **危险操作**（Smashing Magazine 综述）：不可逆=确认对话框或输入名确认；"danger zone"独立分区是 web 习惯；**内联 banner 只承载状态告警**（未运行/磁盘满），不承载破坏性动作——印证 D-18 路线。
- Linear/Notion 轻模式：左分类二级导航+每类内搜索、低饱和高留白。

### 2.2 清单+详情工作台可量化惯例

- **Fluent list/details**：窗口 **<640epx stacked（列表/详情二选一）、≥641 并排**；选中即更新详情。
- **WinUI 行高密度基准**：单行 **44px**（图标16/内边距12）、双行 **64px**、三行 84px、**表格行 48px**（图标32）；表头 Caption 12px+浅底+padding 12；数字右靠、文本左靠、最小点击目标 24px、骨架屏。
- **GitHub Desktop**：非对称双栏（清单窄/diff 宽）+ 底部固定操作表单。
- **Files app / Win11 资源管理器**：页签条+侧栏+主区+详情窗格四段、工具条按空间溢出"…"——与 00-spec「≤3 直出+…溢出」互证。

### 2.3 Fluent/WinUI 官方 token 兼容确认

- 间距官方推荐 8/12/16 与 expander 内缩进 48——与我们 4/8/12/16/24 白名单**完全兼容**（24 以 12+12/16+8 表达）。
- **字阶（Win11 type ramp）**：Caption 12/16、Body 14/20、BodyStrong 14/20 Semibold、Subtitle 20/28、Title 28/36；**正文最小 14、注释最小 12；标题 Semibold 不 Bold**；行长 45–75 字符——00-spec 原缺字号条款，已补入 §9。
- accent 只标状态与可交互；深浅主题自动对比版；正文对比 ≥4.5:1（WCAG）。

### 2.4 中文桌面工具：学什么/不学什么

- 学：Quicker"搜索框永远置顶+动作分组网格+右键直达"、uTools"输入即启动+窄主面板强迫单列扫读"——共相=**一个搜索框解决 80% 入口**；
- 不学（反面教材，样张中一律不出现）：360/鲁大师式加速球常驻、托盘营销弹窗、设置页推广卡片。

### 2.5 来源（UI 调研）

- [Fluent: Guidelines for app settings](https://learn.microsoft.com/zh-cn/windows/apps/design/app-settings/guidelines-for-app-settings) · [list-details](https://learn.microsoft.com/zh-cn/windows/apps/develop/ui/controls/list-details) · [ListView item templates](https://learn.microsoft.com/en-us/windows/apps/design/controls/item-templates-listview) · [content-basics 间距](https://learn.microsoft.com/zh-cn/windows/apps/design/basics/content-basics) · [typography ramp](https://learn.microsoft.com/zh-cn/windows/apps/design/style/typography) · [color/accent](https://learn.microsoft.com/zh-cn/windows/apps/design/style/color) · [accessible text contrast](https://learn.microsoft.com/zh-cn/windows/apps/design/accessibility/accessible-text-requirements)
- [PowerToys 0.94 devblogs（设置搜索/冲突检测）](https://devblogs.microsoft.com/commandline/powertoys-0-94-is-here-settings-search-shortcut-conflict-detection-and-more/) · [VS Code Settings docs](https://code.visualstudio.com/docs/getstarted/settings) · [Windows Terminal 新版设置(ithome)](https://www.ithome.com/0/942/486.htm) · [Smashing: dangerous actions](https://www.smashingmagazine.com/2024/09/how-manage-dangerous-actions-user-interfaces/) · [GitHub Desktop docs](https://docs.github.com/en/desktop/making-changes-in-a-branch/committing-and-reviewing-changes-to-your-project-in-github-desktop) · [Files app 商店页](https://apps.microsoft.com/detail/9nghp3dx8hdx) · [setproduct 数据表设计](https://www.setproduct.com/blog/data-table-ui-design) · [sspai: Linear 设计风格](https://pwa.sspai.com/post/79347) · [Quicker 板碟介绍](https://blog.csdn.net/BG1230521/article/details/146140542) · [uTools 主面板分享](https://www.360doc.cn/article/81522808_1108915371.html)

## 3. 归口登记

- 系统功能扩充 → `07-sys.md` §8（新子面板"卸载与残留/更新管控/工具箱"、矩阵 13–24、ExpertGate 安全网）；动工前按 D-22 规则四登记 **D-31**（五要素：背景=用户三审裁定，决策/依据/代价/验收引用本台账）。
- UI 条款吸收 → `00-ui-layout-spec.md` §9（字阶、行高、设置页列宽、Ctrl+F 搜索、项级恢复默认、About 置底、双栏断点、banner 语义分工）。
- 可预览样张 → `docs/panels/2026-09-19/preview/index.html`（静态 HTML，浏览器直开审核，非应用代码）。
