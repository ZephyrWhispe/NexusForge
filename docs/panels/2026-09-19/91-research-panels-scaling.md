# 91 四审调研台账：全面板功能增量 + 缩放重排规范（2026-09-19）

> 地位：用户直令四审——"也调研一下其他功能面板，增加别的功能面板的功能。另外注意 UI 在缩放前后位置变动问题，生成所有功能面板的预览"。本档是证据台账；落地条款见各面板档 §8 与 00-ui-layout-spec.md §10，预览样张见 preview/ 全模块页。只审不码：任何条目动工前归口既有批次（B0–B8）或 D-31，按方案先行规则执行。
> 剔除纪律：两条只读调研线均先通读对应档 §2/§3/§4/§5/§7，凡重叠者不录；第一方来源不可核实的条目标 **待复核**，不进 §8 正文。

## 1. A 组面板功能增量（剪贴板/代理/密码库/文件/同步/宿主壳）

### 01 剪贴板
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 片段动态变量（日期运算、粘贴时填表、对当前剪贴板做转换） | TextExpander/Text Blaze | 中 | 轻 | — |
| 内容正则捕获屏蔽：命中 OTP/卡号模式整条**不入库**（区别于入敏感库，与 7.2-① 应用维度互补） | Maccy | 高 | 轻（classify 钩子） | 强化红线 |
| 重复复制"置顶不新增"去重开关 | CopyQ/Ditto 默认行为 | 中 | 轻 | — |
| 内容模式→automation 动作建议条（复制单号/URL 弹可执行动作，**建议制非自动执行**） | Quicker | 中 | 轻 | — |
| 本地语义搜索/自动打标（可选 sidecar 默认关） | SmartClipboard（**待复核**，mac 向） | 中 | 重 | 归 B8 统评，单面板不得私引 |

### 02 代理
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| TUN↔系统代理由硬互斥改**可并存互补**（UWP/老程序覆盖差异），冲突需检测提示 | Clash for Windows 社区/FlClash | 高 | 轻中 | — |
| REALITY / XHTTP(SplitHTTP) 传输 + 节点 TLS 细调表单（2024–2026 抗封锁主波） | Xray/v2rayN 系 | 高 | 中（sub.rs+IR+golden） | 畸形配置负例 |
| Windows 服务模式：内核随服务开机起、TUN 恢复免 UAC | clash-nyanpasu 痛点 issue | 高 | 重 | 先入 D-13 提权边界评审 |
| 本地覆写脚本（用户本地 JS 调订阅生成配置）：默认关、仅本地文件、变更 diff 确认 | Clash Verge Rev | 中 | 中 | 防投毒延伸 |
| 分应用代理可视化向导（从运行窗口列表挑选 + 场景预设；§3-12 已有 process 规则，缺 UI 拾取器） | Netch（维护状态**待复核**） | 中 | 轻 | — |

### 03 密码库
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 浏览器一键导入（Chrome/Edge 本地登录库 DPAPI 解密，免手导 CSV） | Bitwarden 迁移路径 | 高 | 中重 | 明文仅内存即用即零化、不落临时文件不进剪贴板（负例） |
| 条目"到期时间"字段 + 临期托盘提醒 | KeePassXC | 高 | 轻 | — |
| 内置 SSH agent（库内私钥服务 git/ssh，锁库即撤） | KeePassXC SSH Agent | 高 | 重 | 私钥不落盘；仅解锁态存活 |
| 多库管理（工作/个人分库、独立锁定与生成器上下文） | KeePassXC 2.7+ | 中 | 重 | 各库独立信封，导出互斥 |
| 仿冒域名拦截：AutoType/打开 URL 前离线编辑距离+同形字告警 | 1Password（2026-01） | 中 | 中 | 纯本地词表，不联网 |

### 04 文件·网盘
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 内容搜索 `content:`（指定范围全文检索，扫描模式显式降级横幅） | Everything | 高 | 中 | 仅扫用户指定目录 |
| 空格键 QuickLook 浮层大预览（图/PDF/视频/压缩包） | QuickLook / Files 4.0 | 高 | 中（复用 file_preview+overlay） | — |
| 归档就地浏览 + 选择性拖出解压（不整包解） | Directory Opus 13 | 中高 | 中 | — |
| 本地↔远端目录 diff/镜像同步（先比对清单→确认→执行） | WinSCP 6.x Synchronize | 高 | 中重（挂 B6） | 镜像删除走确认词+清单预览 |
| 校验和列 / 右键 SHA-256 + 比对报告 | Directory Opus/Files | 中 | 轻 | — |
| 保存搜索=虚拟文件夹 + 每目录视图记忆 | Everything/DO | 中 | 轻（host_config） | — |

### 14 同步
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| NAS/WebDAV 第三端点双向文件夹同步（bisync 式，仍无中继服务器） | rclone bisync | 高 | 重（复用 B6 驱动） | 端点凭据 vault 引用永不入同步流 |
| 冲突策略加"保留两者"：败方快照落 fork 记录再手合（LWW 仍默认） | Syncthing | 高 | 中 | — |
| 条件自动暂停（按流量计费/省电/SSID 非家庭网不出账） | Syncthing-Android | 中高 | 中（网络成本 API 勘查） | — |
| 定期漂移自检（op 摘要对账，报"缺 N 条"+一键重推） | Syncthing db repair | 中 | 中 | 只报计数不报明文 |

### 15 宿主壳
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 场景模式/一键工作台（联动多模块开关+代理 profile+程序组，切换可逆逐模块回告） | PowerToys Workspaces | 高 | 中（总线编排既有命令） | — |
| 命令面板实体化（结果含笔记标题/最近文件/片段触发词，分组直达） | Flow Launcher/uTools | 高 | 中（EntityIndexProvider trait） | vault 命中仅标题、永不自动复制 |
| 通知免打扰时段（焦点时间窗） | Win11 Focus | 中 | 轻 | 负例：指纹变更/内核崩溃必穿透 DND |
| 模块拆出独立窗口（双屏各开一面板常驻） | Files/DO 多窗口 | 中 | 中（Tauri 多窗+状态共享） | — |

## 2. B 组面板功能增量（桌面/键鼠/终端/截图/OCR/编辑/笔记/自动化）

### 05 桌面效率
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 启动器拼音/首字母匹配（`dd`→钉钉），与别名表同一条打分链 | Flow Launcher | 高 | 轻（离线词表） | 禁在线取词 |
| 整理规则维度扩（大小/修改日期/正则/前缀）+ 新落桌面文件自动归入首个匹配组（默认关，每次可撤销） | Stardock Fences | 高 | 中 | 复用 §5-3 时间线撤销 |
| 启动器新结果类型"窗口"（搜窗口标题→切换/置顶） | PowerToys Window Hopper | 中 | 中 | 只读枚举，不注入键鼠（避免与 06 钩子打架） |
| URL 固定项：默认强制 HTTPS、可选浏览器 profile/隐身/新窗 | Flow Launcher URL 插件 | 中 | 轻 | HTTPS 失败回退提示非静默 |
| 双击桌面空白隐藏图标（Fences 经典交互） | Fences（**待复核**） | 中 | 中 | 改用户桌面状态：capability+可逆+一次性确认 |
| 活文件夹分区 / 图标着色角标 | Fences 6（**待复核**） | 低–中 | 重/轻 | 登记**不采纳**（价值低、侵入图标本体风险） |

### 06 键鼠共享
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 滚轮速度缩放系数（注入前乘 delta，默认 1.0） | Deskflow v1.26 | 中 | 轻 | 与冷却/容差同族进设置 |
| 按键重复语义透传（长按退格/方向键对端持续重复；总开关关/锁屏后重复计数须为 0 负例） | Deskflow v1.26 | 高 | 中 | 密码类输入不进共享通道 |
| 切机瞬间死键与修饰键状态复位（对 §7 修饰键同步的深化：补死键复位） | Input Leap 3.0.3 | 高 | 中 | — |
| 边缘穿越落点连续性（按相对位置落点，不跳屏幕中心） | Input Leap 3.0.3 | 高 | 中（edge_map 坐标映射） | 与 §10 缩放换算同批回归 |
| 跨机剪贴板 UTF-16 代理对（emoji/生僻字）往返回归 + 负例 | Input Leap 3.0.3 | 中 | 轻 | 类型开关与大小上限不变 |
| "锁定到本机"瞬时冻结（托盘项+热键；默认不锁，锁定态概览大字卡可见） | Deskflow v1.26 | 中 | 轻中 | — |
| 对端地址多候选（主机名优先 IP 回退）+ 本机建议地址随网络更新 | Deskflow v1.26 | 中 | 中 | 仅本地网段解析，不做外部 DNS/云发现 |

### 08 终端运维
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 命令块 Blocks（命令+输出成块：块级复制/时间戳/退出码/跳上一块；无 shell integration 降级整屏复制，禁假就绪） | Warp | 高 | 中重（OSC 133 即 §7.2-6 落点） | — |
| 输出触发器（正则匹配远端输出→高亮/告警/发 `term.output_matched` 事件交 13） | WindTerm Trigger | 高 | 中 | 默认只提示不自动执行；触发词含明文凭据不入同步 |
| 登录模板（一批主机共享登录设置，改一处全生效；只存 vault 引用/密钥路径） | WindTerm OneKey | 中 | 中 | — |
| 分屏"复制当前会话"（同主机同目录新 pane）+ 分屏布局命名 | Ghostty/WT（**待复核**措辞） | 中 | 中 | — |
| tmux 控制模式集成（`tmux -CC` 远端窗格映射本地标签） | WindTerm 2.7 | 中 | 重 | 挂 v1.1 评估，不做不承诺 |
| 全局热键"快速终端"下拉窗（复用 ConPTY） | Ghostty quick terminal（**待复核**） | 中 | 中 | 与截图覆盖层抢焦点须登记互斥 |
| （WT 1.24 SSH config 导入=impl/09 B7⑥既有，此处仅补来源：Windows Terminal Preview 1.24） | — | — | — | — |

### 09 截图与贴图
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 重复上一次捕获（上次选区/上次窗口/全屏三级重放，与延迟截图共用命令入口） | CleanShot/ShareX | 高 | 轻 | — |
| 导出前元数据清洗（EXIF/GPS/创建软件）+查看元数据卡，默认"上传/外发前清除" | ShareX v17 Metadata | 高 | 轻中 | 关闭须在任务流可见 |
| 混合 DPI 逐屏独立覆盖层（每屏按自身 scaleFactor 建层） | PowerToys Text Extractor | 高 | 中（§3-10 monitors[] 同批） | 与 §10 CSS×dpr 换算同批回归 |
| 两图对比（历史选两张→差异高亮/并排滑块，纯本地） | ShareX v21 Comparer | 中 | 中 | 不做以图搜图外链 |
| 光标与点击可视化开关（默认不含指针；叠加帧不落盘） | CleanShot | 中 | 中 | — |
| 贴图鼠标穿透 + 置顶层级可选（穿透态留可见角标） | PixPin/Snipaste（**待复核**措辞） | 中 | 中 | — |
| 窗口区域"实时钉图"（活贴图持续刷新） | PowerToys Crop And Lock | 中–高 | 重 | 帧率节流；密码管理器/银行页默认不捕 |
| （像素级放大镜：impl/09 B4②既有，本轮升级为 8–12× + 像素网格 + 坐标/颜色读数） | Snipaste/PixPin | 高 | 轻中 | 帧不外拷不二次驻留 |

### 10 OCR
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 文本方向自动纠正（低置信按 90/180/270 旋转重试取最优；仅整图旋转不加引擎依赖，不违 D-08） | PaddleOCR 方向分类思想 | 高 | 中 | — |
| 扫描件回写"可搜索 PDF"（隐藏文字层；默认另存不原地覆盖） | Stirling-PDF | 高 | 重 | 与 11 共用 |
| 本地识别服务（HTTP 仅回环+CLI）：默认关、只绑 127.0.0.1、请求图不入历史 | Umi-OCR HTTP API | 中 | 中 | 共用 §10 本地端口门禁 |
| 忽略区域模板（按比例存，防换分辨率错位） | Umi-OCR | 中 | 轻 | — |
| 批量按内容类型分组归档（表格/代码/无文本/疑似敏感四类出口；敏感类默认打码不外发） | Umi-OCR+本仓 §5 组合 | 中 | 中 | — |

### 11 文本与 PDF
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| PDF 元数据编辑与隐私清洗（作者/标题/生成器/日期/GPS；导出预览明示内容） | Stirling-PDF sanitize | 高 | 轻中 | 不可逆→确认词；沿用 §7.2-5 |
| PDF→Markdown/结构化导出（保留标题层级与表格，直落笔记 assets） | Stirling/MarkItDown | 高 | 中 | assets 路径消毒负例 |
| 多工具流水线（N 文件×多步骤串联一次出结果；步骤失败不级联、中间产物可见可清） | Stirling multi-tool | 中–高 | 中 | — |
| 页码/页眉页脚 + 删除空白页 | Stirling 页面族 | 中 | 中 | 默认另存（§5-5 沿用） |
| AcroForm 表单填写 + 本地图片签名章 | Stirling fill/sign | 中 | 重 | 签名图本地选取绝不上传；不做云验签 |
| 大文本按行窗口流式只读（>50MB 可跳行号，只读态显性化） | Notepad++ 心智 | 中 | 中 | §7.2-4 沿用 |

### 12 笔记与知识
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 属性数据库视图（frontmatter 当列；表格/卡片/看板多已保存视图+公式列；视图定义存库内可同步） | Obsidian Bases | 高 | 重 | 不引在线模板市场 |
| 跨库任务查询（全 `- [ ]` 按到期/标签聚合，勾选回写原文件） | Obsidian Tasks | 高 | 中 | 原地改写 D-18 确认；与 05 待办**单源真相** |
| 内嵌引用 `![[笔记#小节]]`/`![[图片]]` 就地渲染（递归深度上限防环，死链急救同分支） | Obsidian transclusion | 高 | 中 | — |
| 有限富文本：`==高亮==`（导出仍纯文本） | Obsidian | 中 | 轻 | 禁私有二进制格式入文件 |
| 大库模糊跳转（Ctrl+P，>1 万文件可用；与 05 共用打分层） | Obsidian changelog | 中 | 轻中 | — |
| 多库切换（每库独立根/附件策略/信封；切库先锁） | Obsidian vault switcher | 中 | 中 | 密钥不混用；同步按库登记 |
| 整库导出离线静态 HTML（只出本地目录） | Quartz | 中 | 重 | 不内置发布/账号 |

### 13 自动化
| 功能 | 来源 | 价值 | 深浅 | 红线 |
|---|---|---|---|---|
| 规则版本历史+一键回滚（保存前快照+diff 列表；仅本机，不入同步） | n8n history | 高 | 中 | 导入不覆盖历史 |
| 子规则（步骤转可复用子规则、删除前反查引用） | n8n sub-workflow | 中–高 | 重 | 递归静态检测扩展 §5-3；子规则不得超出所在窗 capability |
| 失败路由规则（指定专收其他规则失败；自身失败必须停防风暴） | n8n error handling | 中 | 中 | — |
| 规则标签（过滤/批量启停，随 JSON 导出） | n8n tags | 中 | 轻 | 导入冲突沿用 §7.2-5 |
| 步骤级禁用 + sticky 注释（注释不得成为可执行内容） | n8n | 低–中 | 轻 | — |
| 实时执行视图（运行中高亮当前步+耗时；只读，禁从此改参） | n8n live view | 中 | 中 | 与 dry-run 严格二分 |
| "下次运行"总览日历（定时规则时间轴+错过补偿标记；不承诺唤醒） | n8n Schedule | 中 | 轻中 | §7.2-1 诚实文案沿用 |
| 键鼠录制生成规则 | PAD 记录器 | 低 | 重 | **登记不采纳**：不做后台 UI 自动化注入 |

## 3. 跨面板共性发现（A+B 合并，归口建议）

1. **统一匹配打分层**：拼音首字母+模糊+别名/`{q}` 模板应为一处公共 crate——05 启动器、12 跳转、01 搜索、15 命令面板共用，禁各面板各写 ranking。
2. **"时间/到期"调度缺宿主承接**（03 条目到期、02 订阅到期、05 提醒、13 下次运行）——宿主级"时间事件调度→通知中心深链"统一承接，勿各造定时器。
3. **"干跑预览→清单确认→执行"升格通用组件**（04 镜像同步、02 覆写脚本、03 浏览器导入、15 场景切换四处再现）——并入 README §0.3 组件族（与 00§9-10 破坏性 ConfirmDialog 同族）。
4. **隐私外发前置清洗管线**：元数据清除（09/11）、敏感打码（10）、水印（09 §7.2-2）、导出 diff 预览（08/13）抽为宿主级"外发前检查"步骤，各面板只登记内容类型。
5. **本地端口统一门禁**：OCR 回环服务（10）、代理（02）、文件共享（04）共用条款"默认关 + 仅 127.0.0.1 + 无凭据 + 端口自检"（14 的 49820 自检同族）。
6. **"上次状态记忆"单源**：上次选区（09）、标注默认（09）、分屏布局（08）、当前 profile（06）、zoom 档位（§10）统一走 host_config，禁各模块私存。
7. **事件目录扩面**：`term.output_matched`、`screenshot.completed`、`kvm.edge_switched`、`ocr.batch_completed`、`notes.rename_propagated` 全部进 13 的事件目录，automation 为唯一宏引擎。
8. **本地 AI 增强**（语义搜索/自动打标）统一裁定：可选 sidecar+默认关+内容不出机，归 B8 一并评估，单面板不得私引。

## 4. 缩放与自适应布局调研（落 00-spec §10 的证据）

仓库现状证据：主窗 `1280×800 / minWidth 940 / minHeight 600`（src-tauri/tauri.conf.json:17-20）；Fluent v9 令牌 px 制（src/theme/theme.ts）；Tauri v2 已有 `window.scaleFactor()`、`Webview.setZoom()`、`zoomHotkeysEnabled`（需 `webview:allow-set-webview-zoom` 权限）；覆盖层已做 `CSS px × dpr` 物理换算（src/windows/OverlayShot.tsx:27,953）。

三条最可能根因（需实测确认）：①覆盖层/贴图 CSS px 与物理 px 混用且 scale-changed 后未重算；②minWidth 940 落在 WinUI Medium 断点（641–1007）内却无该档确定形态；③内容列/设置页未预留 scrollbar gutter，条目增减整页左右漂移。

条款化结果见 00-ui-layout-spec.md §10（16 条），主要来源：

- Windows 断点与 4 倍数：https://learn.microsoft.com/en-us/windows/apps/design/layout/screen-sizes-and-breakpoints-for-responsive-design
- NavigationView 折叠阈值 640/1008：https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/navigationview
- Win32 高 DPI（DPI 敏感数据重估、位图重栅格化、系统建议几何）：https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows
- 命中区目标尺寸：https://learn.microsoft.com/en-us/windows/apps/develop/input/guidelines-for-targeting
- CSS zoom 参与布局 vs transform:scale：https://developer.mozilla.org/en-US/docs/Web/CSS/zoom
- scrollbar-gutter：https://developer.mozilla.org/en-US/docs/Web/CSS/scrollbar-gutter
- overflow-anchor：https://developer.mozilla.org/en-US/docs/Web/CSS/overflow-anchor
- tabular-nums：https://developer.mozilla.org/en-US/docs/Web/CSS/font-variant-numeric
- prefers-contrast：https://developer.mozilla.org/en-US/docs/Web/CSS/@media/prefers-contrast
- devicePixelRatio：https://developer.mozilla.org/en-us/docs/Web/API/Window/devicePixelRatio
- VS Code zoom 与字体二分：https://code.visualstudio.com/docs/getstarted/appearance
- PowerToys "stable item widths"：https://github.com/microsoft/PowerToys/releases
- TanStack Virtual measures（**待复核**，仅实现细节参考）：https://tanstack.com/virtual/latest/docs/introduction
- Slack/GitHub Desktop zoom 具体做法（**待复核**，不作条款依据）

## 5. 归口登记

- 各面板档 §8「四审功能增补」：01/02/03/04/05/06/08/09/10/11/12/13/14/15（07-sys 三审已扩充，本轮无新增）。
- 00-ui-layout-spec.md §10：缩放与自适应 16 条。
- preview/ 重构为共享样式 + 每模块一页 + 每页三档宽度切换（1280/960/640），演示 §10 断点行为；几何自检扩展到多宽度。
- 批次映射：各 §8 表内"批次"列指回 B0–B8；重依赖/特权面条目（服务模式、DPAPI 导入、SSH agent、tmux -CC、本地 AI）动工前须 D-13 边界评审或 B8 裁决；不采纳项（键鼠录制、活文件夹/图标着色、以图搜图）在本档留痕不再讨论。
- 新命令一律 D-28 capability 同步 + security_config.rs 兜底；红线批必含负例。
