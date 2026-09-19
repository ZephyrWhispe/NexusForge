# 04 文件·网盘·FTP/WebDAV/HTTP 面板详细设计（B1+B6）

> 总纲：docs/impl/09 §6；对标：Oxyde_FM/Files（蓝本 §3.4）+ OpenList（§3.5）+ AeroFTP/Filestash（§3.6）；借 FileZilla/Total Commander/Everything/Directory Opus/WinDirStat；状态：未开工。

## 1. 现状问题

- 孤儿命令 7 条：`file_search`（USN 秒搜+降级标记）、`file_preview`（图片/文本预览）、`file_rename_plan/apply/entry`（批量重命名 DSL）、`file_drivers`（驱动树）、`file_ops_pending/drop_pending`——**副标题已对外承诺"搜索 · 批量重命名"而 UI 不存在**（MainWorkbench 文案与实现脱节，登记）。
- UI 一屏竖排：面包屑+操作条+冲突框+队列+目录表全叠在一起；无标签页、无双面板、无预览窗格（蓝本 §3.4 三大 UI 卖点全缺）。
- compress/extract 枚举在 OP_KIND_LABEL 里但面板永远发不出这两种任务（假就绪面）。
- 远端协议：file-core `driver.rs:5-6` 仅注释，零实现；服务商预设（蓝本 AeroFTP 44 预设）无。

## 2. 子面板信息架构

| 子面板 | 类型 | 内容 |
|--------|------|------|
| 文件 | 清单 | 标签页条（每 tab 独立历史栈）+ 可切双面板 + 目录表 + 右可折叠**预览窗格**（文本/图片/基本信息）+ 底部状态行（选中 n 项 / 共大小）|
| 传输 | 清单 | 活动队列（进度/暂停/取消/移除）+ 挂起任务（ops_pending 恢复）+ 近期完成历史（速率、用时、失败重放）|
| 搜索 | 清单 | 查询框（即时去抖）+ 过滤器面（类型/大小/日期/盘）+ 结果表（路径中段高亮、**degraded 降级横幅**"USN 不可用，逐目录遍历中"）+ "用选中位置打开文件面板" |
| 批量工具 | 编辑 | 批量重命名工作台：规则输入（DSL `{name}{n:03}`/正则/大小写）+ **左旧右新实时预览表** + 冲突列 + 应用（带撤销日志）；压缩/解压卡片（选文件→格式→选项）|
| 远程连接 | 清单+编辑 | 站点管理器（FileZilla 心智）：连接列表（协议图标/别名/最近使用）+ 新建/编辑表单 + **预设模板**下拉（WebDAV 坚果云/S3 兼容端点清单等 JSON 数据文件）+ 双击进"文件"子面板远端模式 |
| 网盘 | 清单 | rclone 驱动挂载列表（依赖 D-08 sidecar 通道就绪）；就绪前整页 DeferredBadge |
| 设置 | 设置 | 预览大小上限、删除是否走回收站、默认列显示、双击行为、隐藏/系统文件开关 |

## 3. 对标功能矩阵

| # | 功能 | 来源 | 现状 | 实施 |
|---|------|------|------|------|
| 1 | 多标签页（独立历史栈） | 蓝本 §3.4 | ❌（editor 有 sessions，file 无）| 前端 tabs + 后端每 tab 栈（本地即可，导航历史纯前端）|
| 2 | 双面板/多面板拖拽 | 蓝本/TotalCommander | ❌ | 一期做"双面板开关"（对分栏），拖文件跨栏=移动/复制（修饰键定）；动态多面板列拓展 |
| 3 | 预览窗格 | 蓝本/Files | 后端✅孤儿 | file_preview 接线 + 空间/文本/图片三渲染 |
| 4 | 异步队列/暂停/恢复/取消/断点 | 蓝本 | ✅ 强 | — |
| 5 | 冲突 覆盖/跳过/重命名/自动编号 | 蓝本 | ✅ | 队列页冲突挂起项可事后决议（pending 命令接线）|
| 6 | 搜索秒搜+降级 | 蓝本/Everything | 后端✅孤儿 | 搜索子面板 |
| 7 | 批量重命名 DSL+正则 | 蓝本/Directory Opus | 后端✅孤儿 | 工作台 + 撤销（rename journal 新表）|
| 8 | 回收站删除 | ✅ | — | 设置开关 |
| 9 | `TransferProtocol` 统一抽象 | 蓝本 §3.6 | ❌ | B6 核心：会话对象（connect/list/upload/download/delete/rename）挂 StorageDriver 适配进 DriverRegistry → 远端在"文件"子面板透明浏览 |
| 10 | FTP/FTPS suppaftp、SFTP russh-sftp、WebDAV reqwest、HTTP 下载 | 蓝本 §3.6 | ❌ | 四协议实现；SFTP 复用 term 的 russh 依赖栈 |
| 11 | 网盘驱动注册表（多存储） | 蓝本 §3.5 OpenList | 注释位 | rclone sidecar（下载通道复用 proxy kernel 模式）；v1 经 rclone 后端覆盖阿里/腾讯/OneDrive 等，**不逐厂适配**（登记）|
| 12 | 服务商预设模板 | 蓝本 AeroFTP 44 预设 | ❌ | `presets/*.json` 数据文件 + 表单快填 |
| 13 | 凭据保存 | FileZilla | ❌ | 红线：口令不落明文——复用 vault 信封或独立加密 store，UI"保存到密码库"选项明示；连接串导出永不带密码（负例）|
| 14 | 在线预览编辑/分享链接（Cloudreve） | 蓝本 §3.5 亮点 | ❌ | Web 服务端思想不适配桌面本地优先——**登记不做分享服务**；预览复用本地预览链 |
| 15 | 文件色标/标签 | Files | ❌ | 拓展 §5-② |

## 4. 其他软件借鉴

- **FileZilla 站点管理器**：分组树 + "最近的站点" + 导入 filezilla.xml；
- **Total Commander**：命令行式路径直达 + 按扩展名关联打开方式；
- **Everything**：搜索框函数式过滤语法（`ext:rs size:>1mb` 子集）；
- **WinDirStat/TreeSize**：磁盘 treemap——落 07-sys"存储分析"，本面板"属性"给单目录大小；
- **OpenList**：驱动自注册模式（meta+reader 强制、可选能力接口）——StorageDriver trait 已近，扩 `Put/Move/Quota` 可选接口。

## 5. 拓展设计（常人未思）

1. **地址魔术栏**：面包屑栏直接粘贴任何字符串自动判型——本地路径/UNC/`sftp:// webdav:// ftp://` URL→现场建远程连接并进入（Filestash 心智：URL 即入口）。
2. **文件色彩标签**：右键打色标（写 ADS `NexusForge.Tag` 或 sidecar 数据库，选型勘查登记），SubNav 加"按标签"过滤——Files.app 最受好评的轻功能。
3. **重复文件清道夫**：按大小分组→哈希精验的扫描工作台（可并行复用队列），勾选批量删（回收站）——"磁盘满了"场景闭环。
4. **目录监听→事件**：对指定目录变更发 `file.changed` 总线事件，automation 可订阅（如"下载目录出现 exe→扫描提醒"）。
5. **剪贴板文件联动**：复制了文件再打开剪切板历史，条目直接显示缩略图与"在文件夹中打开"（file+clipboard 双模块已在的数据，缺渲染桥）。

## 6. 验收点

- 7 条孤儿命令全部有 UI 入口且副标题文案与实现一致（文案回归测：承诺词表与命令接线表交叉断言）；
- 四协议各带测试桩（tiny-http WebDAV 桩/russh 测试服务器/suppaftp mock）；畸形响应/超大目录分页负例；
- 凭据红线：任何导出/日志/错误消息不含密码（字符串负例夹具）；
- 重命名预览=apply 结果一致性测（含正则回溯拒绝的恶意 pattern）；
- 预览内存上限（500MB 文本只读前 N KB，负例不 OOM）。
