# 02 代理与 VPN 面板详细设计（B2）

> 总纲：docs/impl/09 §5；对标：v2rayN/IRBox/FlowZ（蓝本 §3.2）+ Clash Verge Rev/NekoBox/Hiddify；状态：未开工。

## 1. 现状问题

- 后端：单内核锁死三点（`service.rs:463` 硬编码 SingBoxDriver、`config.rs` sing-box 方言无 IR、`sub.rs` 协议枚举仅 ss/vmess/trojan/vless）；`delay_test` 是 TCP RTT 伪测速（service.rs:515）；路由仅 ip_is_private+后缀直连（config.rs:61）；无节点选定命令（面板节点表只读）；无内核重启命令。
- UI（478 行一屏竖排到底——"一锅炖"最重灾区）：残留提示条/模式/内核/订阅/节点/直连规则/日志 7 块全部纵向堆叠；节点无分组无选择操作；订阅无流量/到期信息位；无统计图表；日志无级别过滤与清空。

## 2. 子面板信息架构

| 子面板 | 类型 | 内容 |
|--------|------|------|
| 总览 | 概览 | 大开关（系统代理/TUN 互斥）、当前内核+节点、上下行速率 sparkline、代理连通自检灯、残留恢复提示 |
| 节点 | 清单 | 按订阅分组 Tab + 全部/收藏视图；列：名称/协议/组/延迟(历史浮窗)/掉落率；行操作：选定/测速/收藏/编辑（单选覆写）；顶栏：手动选定↔自动优选切换、排序（延迟/名称/协议） |
| 订阅 | 清单 | 订阅卡列表：名称/URL（脱敏）/节点数/**流量剩余与到期日**（sub 响应头解析）/更新策略（手动/间隔/启动时）/按钮：更新、复制、删除；"从剪贴板导入链接" |
| 分流 | 编辑 | 模式（全局/规则/直连）+ 规则表（type: domain/suffix/keyword/cidr/geosite/geoip/process + 目标: 代理/直连/拒绝/指定节点组）+ 内置预设（大陆常用/仅绕过国内/GFW 清单只读预览）+ 导入 Clash 规则片段 |
| 内核 | 概览+设置 | 内核选择卡（sing-box/xray/mihomo：版本、安装状态、切换按钮、换核影响提示）；TUN 设置（栈 system/gvisor/mixed、strict route、MTU）；DNS 设置（远端/本地 DNS、嗅探开关）；mixed 端口、日志级别 |
| 日志 | 历史 | 内核日志流（级别过滤、搜索、暂停滚动、清空、导出）+ 连接日志（若内核 API 可得：目的域名/字节数/所选节点——mihomo/sing-box API 勘查项） |

## 3. 对标功能矩阵

| # | 功能 | 来源 | 现状 | 实施 |
|---|------|------|------|------|
| 1 | 内核抽象 CoreProvider | 蓝本 | trait 在但单实现 | B2-1 配置 IR 先行（golden 等价）；trait 升格 config_render/supported_protocols |
| 2 | xray/mihomo 第二三内核 | v2rayN/FlowZ | ❌ | XrayDriver+MihomoDriver（sidecar 参数化下载、SHA256 记录复用 sing-box 模式） |
| 3 | 换核生命周期（停旧→重生成→起新→重挂系统代理/TUN） | v2rayN | ❌ | service 状态机；负例：换核中途失败回滚旧核 |
| 4 | 订阅 URI 全家桶 ss/vmess/trojan/vless | 蓝本 | ✅ | — |
| 5 | hysteria2/tuic5/wireguard/ssr | v2rayN/NekoBox | ❌ | sub.rs 扩展，每种协议畸形负例 |
| 6 | Clash YAML 订阅/配置导入 | Clash Verge | ❌ | 白名单子集解析（proxies/proxy-groups/rules），拒任意扩展字段（防投毒红线） |
| 7 | 订阅流量/到期头解析 | v2rayN | ❌ | `proxy_sub_update` 返回值扩 `TrafficInfo{upload,download,left,expire}` |
| 8 | 节点分组/收藏/排序/搜索 | v2rayN | ❌ | 本地覆写表（node_overrides：favorite/sort 字段）+ UI |
| 9 | 手动选节点 / urltest 自动优选 | 全体 | ❌ | 新命令 `proxy_node_select`/`proxy_node_auto`；IR Selector/Urltest 组真实化 |
| 10 | HTTP 204 真实测速 + 历史 | Clash Verge | TCP RTT 伪 | delay_test 改造 + 每节点延迟环形缓冲（总览/节点浮窗用） |
| 11 | geoip/geosite 分流 | 全体 | ❌ | geodata 下载（sidecar 通道）+ 三内核各自规则方言渲染 |
| 12 | 进程规则分流（按程序名走代理） | sing-box/xray process_name | ❌ | IR Rule 加 process；UI 分流页"应用"类型行 |
| 13 | TUN 模式 | ✅ | 在 | 补栈选择/MTU/strict route 设置位 |
| 14 | Pre-Socks 双核协作 | v2rayN | ❌ | 总纲明示二期（B8 子方案） |
| 15 | 系统代理兜底恢复 | FlowZ | ✅ 实现（DOC-12：原引 D-06 系笔误——D-06 是 blob 扁平路径；本项按 PR4 备份-识别-还原落地） | — |
| 16 | 内核启停/重启按钮 | 全体 | ❌ | `proxy_kernel_restart`（新命令+权限 ACL 同步） |
| 17 | 带宽/流量统计 | Clash Verge/AureStream | ❌ | API 轮询（mihomo /traffic ws、sing-box experimental API；勘查后定，缺则降级"仅连接自检"并如实挂 DeferredBadge） |
| 18 | 多用户配置方案(profile 切换) | v2rayN profiles | ❌ | 拓展项 §5-③ 承接 |

## 4. 其他软件借鉴（蓝本外）

- **Clash Verge Rev**：侧栏延迟迷你图、"代理模式"三圆点常驻托盘、TUN 栈设置分组样式；
- **Hiddify**：一键"导入并应用"剪贴板链接的引导浮层（新手路径）；
- **NekoBox**：节点"仅绕过大陆"等模式快捷组——落分流预设；
- **v2rayN Wiki（UI 说明页）**：节点右键菜单全集（测试全部、复制服务器信息）——落节点行菜单。

## 5. 拓展设计（常人未思）

1. **代理体检页**（总览"自检"展开）：DNS 泄漏测试（查询返回 IP 对比出口 IP）、WebRTC 泄漏提示、时区/UA 一致性、直连国内站点连通性——每一项一个 pass/fail 灯，红项给修复按钮（自动改设置）。安全心智的制高点，市面客户端几乎无集成。
2. **故障现场保全**：内核异常退出时自动打包（最后 200 行日志+配置脱敏版+系统代理态+内核版本）落 crash 目录，面板挂"上次异常退出·查看现场"。脱敏红线：节点密码/uuid 必须遮蔽（负例测试）。
3. **配置方案 Profile**：多套"订阅+分流+内核+TUN"组合一键切换（公司/家用/实验）——复用 host_config 存储。
4. **规则命中追踪**：开启后连接日志显示"命中规则 #n（来源）"，分流页规则行显示今日命中计数——把"为什么这个站没走代理"变成可查问题。
5. **订阅变更 diff**：更新订阅时列出"新增/移除/延迟劣化"节点摘要，防止节点被静默替换（安全向）。

## 6. 验收点

- 三内核 golden-file（IR→各方言快照）+ IR 非法组合拒；重构前后 sing-box 输出一字不差；
- 换核 e2e（测试双驱动）：中途失败回滚断言；系统代理/TUN 态不破；
- 订阅解析新增 4 协议 + Clash YAML：每族畸形/投毒负例；
- 子面板化后主区任一层滚动深度 ≤2；日志过滤/清空/导出可用；
- 新命令（node_select/node_auto/kernel_restart/traffic）全部同步 permissions+main capability。

## 7. 排版方案与二审补充（2026-09-19 复审）

### 7.1 排版方案

- **总览（概览）**：上部横排两大卡 `[系统代理/TUN 大开关卡]` `[连通自检卡（灯+体检按钮）]`（等宽两列，间距 16）；中部三小卡（当前内核/当前节点/上下行 sparkline）；底部"上次异常退出·查看现场"横幅（条件出现）。大开关=00§6 SwitchSetting 的加粗变体，说明行含互斥提示。
- **节点（清单）**：Toolbar 左=搜索 + 订阅分组 Tab + 协议芯片；右=「测速全部」「手动↔自动」切换钮 + 「新建节点」primary。行=`[收藏☆][名称 ellipsis][协议 Badge][组 Badge][延迟数字右靠(色阶:绿<150/黄<400/红/超时—)][掉落率 %][操作 hover 钮组]`；延迟列固定宽 72 数字右对齐（00§2 防抖宽）。行 hover 浮出 60 点延迟迷你图（点击钉住）。
- **订阅（清单）**：卡片墙两列；卡头=名称+更新策略 Badge；卡内字段行 `标签(灰,定宽 96px)：值` 左缘对齐一条基线；按钮排右下 `[更新][编辑][删除danger]`。
- **分流（编辑）**：顶部模式三圆点 RadioGroup（全局/规则/直连，00§3 ≤5 项禁 Dropdown）；规则表行=`[拖拽把手][启用 Switch][类型 Dropdown S][值 Input L][目标 Dropdown S][删✕]`，**新行永远追加表尾+自动聚焦**（防手滑插中间）；内置预设以"只读卡"呈现，"应用预设"走 ConfirmDialog 说明覆盖范围。
- **内核（概览+设置）**：三内核卡横排（当前=高亮描边）；下方设置分组按 **00§6 依赖序**：端口与协议族 → TUN → DNS → 高级参数（7.2-①）；**内核不支持的参数组整组折叠并在卡上标"由内核能力表决定"**——禁"能看见点不动"。
- **日志（历史）**：工具条左=级别芯片+搜索；右=暂停滚动/清空/导出；主体等宽字体虚拟滚动。

### 7.2 配套功能缺口（并入分流/内核子面板与 §3 矩阵）

1. **高级参数组（IR 一等公民）**：mux 复用、TCP/UDP 超时、xray `fragment` 抗 DPI、utls 指纹——按内核能力表渲染（sing-box/xray/mihomo 支持集不同），IR 加 `AdvancedParam` 命名空间 + 不支持即不渲染（不是禁用）；每项 tooltip 一句人话用途。
2. **LAN 共享（allowlan）**：内核页开关 + 监听地址变更明示（`127.0.0.1→0.0.0.0` 红字警告 + 可选鉴权用户名密码，凭证走 vault 引用）——手机蹭代理是 v2rayN 高频场景，无鉴权裸开是事故。
3. **外部控制 API**：Controller 本地监听开关 + 随机 token 展示/重置（mihomo REST/sing-box experimental API 映射）；默认关；capability 与 CSP 不受影响（纯 Rust 侧监听）。
4. **启动自动连接 + 开机静默启动**：设置两开关（启动时自动起内核/自动开系统代理），与宿主"启动与托盘"（15§2）同一 schema 不同归属——代理侧只管"自动连接"。
5. **兜底 final 规则**：分流表尾固定不可删的 final 行（代理/直连/拒绝三选，默认=按 GeoIP）——"所有规则都不命中怎么办"必须有答案，杜绝未定义行为。
6. **DNS 模式三选**（fakeip / 真实 IP / 仅远端）与泄漏测试联动：体检页红项→一键跳到对应 DNS 设置锚点（00§6 gear 归位纪律的示范）。

### 7.3 锦上添花

- 节点卡"分享配置"：生成**去密码化**二维码（host/port/协议明面，psk/uuid 打码）供局域网临时设备手抄；
- 总览"今日流量按订阅分组"堆叠条；延迟历史导出 csv；
- 内核切换向导（换核前检查：订阅协议兼容性预检——xray 不认 hysteria2 时先报告后切换，防"换核即全灭"）。

## 8. 四审功能增补（2026-09-19，台账 91 §1；批次=B2）

| # | 功能 | 来源 | 深浅 | 归口与红线 |
|---|------|------|------|-----------|
| 1 | **TUN↔系统代理由硬互斥改可并存互补**（UWP/老程序路由差异），并存时冲突检测+提示 | Clash for Windows 社区/FlClash | 轻中 | B2 内核总览的开关语义重做；文案给出"谁接管哪类流量" |
| 2 | **REALITY / XHTTP(SplitHTTP) 传输** + 节点 TLS 细调表单（2024–2026 抗封锁主波，§3-5 未含） | Xray/v2rayN 系 | 中 | 走 B2 配置 IR + golden；畸形传输参数负例；订阅协议兼容性预检（§7.3 向导）联动 |
| 3 | Windows **服务模式**：内核随服务开机起、TUN 恢复免 UAC（现仅"开机自启"开关） | clash-nyanpasu 痛点 | 重 | 触 D-13 提权边界清单评审后定档（B2 尾或 B8 裁决），评审前挂 DeferredBadge |
| 4 | 本地覆写脚本：用户本地 JS 调订阅生成配置 | Clash Verge Rev | 中 | **默认关、仅本地文件、执行前变更 diff 确认**（防投毒延伸至脚本通道） |
| 5 | 分应用代理可视化向导：从运行窗口列表挑选进程→生成 process 规则 + 场景预设 | Netch（维护状态待复核） | 轻 | §3-12 process 规则已列，本条只补 UI 拾取器；B2 节点/规则清单内 |

## 9. 五审增补：内核与分流规则自动更新（用户点名，方案与验收全文在 [docs/impl/10-artifact-auto-update.md](../../impl/10-artifact-auto-update.md)，批次 B9，D-32 预留）

| # | 条目 | 一句话设计 | 落点 |
|---|------|-----------|------|
| 1 | **sing-box 内核自动更新**（"看重的核心"第一行） | 日检 release→官方 checksum 校验→staged 先跑 `sing-box version` 健康→空闲窗口停核原子换装→起核失败自动回滚 .bak；策略三态 关/手动/自动（默认自动） | 10§4.3/§4.4；概览内核卡状态点（10§6） |
| 2 | **geoip.db / geosite.db 自动更新** | 独立构件行（sing-geoip/sing-geosite 仓 date 版本），装到 `{appData}/proxy/geo/`，B2 配置 IR 的 geo 字段引用该目录 | 10§3 表行 3 |
| 3 | **分流规则自动更新=订阅轨** | 订阅拉取读 `profile-update` 响应头得 interval（无头=永不自动）；到期自动刷新→解析→**偏离度门禁**（节点数变化≥50% 或清空→转人工确认，宁旧勿毒）→应用+活动记录；订阅编辑弹窗加"自动刷新：跟随订阅头/关闭/每 N 天" | 10§4.4；sub.rs `SubscriptionMeta` |
| 4 | **手动下载兜底**（用户"或者手动下载之类的"） | `artifact_install(id, version?, allow_downgrade?)`：任意历史版本手输安装；降级必须显式标志+D-18 输入构件名确认 | 10§5 表 |
| 5 | wintun 运行时 | 默认手动（更新极罕见、无官方校验文件→TOFU pin） | 10§3 表行 2 |
| 6 | **与 B2 合流** | xray/mihomo 内核与其 geo/rule-set 一律注册进 artifact-core，禁写内核特例更新器；mihomo 优先接内核原生 `geo-auto-update`/rule-provider `interval`，应用轨只兜底 | 09 §5 第 10 条 |

验收随 10§9（A1–A10），红线负例（镜像投毒/zip-slip/自动降级/aux 窗调 install）同文 §7；本批纯增量，不动 B2 的 IR 与驱动接口。
