# 07 系统管理面板详细设计（B1 接线 + 深化）

> 总纲：docs/impl/09 §7；对标：NeoHtop（蓝本 §3.12）+ UniGetUI（§3.8）+ WinUtil + TreeSize/Autoruns；状态：未开工。

## 1. 现状问题

- 四 Tabs（监控/清理/包管理/调整）已有，但**蓝本 §3.12"进程管理：列表、排序、搜索、结束进程"整块缺失**（metrics.rs 只有 PDH 采样环，无进程枚举）。
- 孤儿命令：`sys_clean_targets`（清理目标管理）、`winops_catalog`（43 项目录）——scan 结果之外无浏览面，用户看不见"能调什么"。
- 包管理动作集只有 install/uninstall/upgrade_all（`pkg.rs:213/315/391`）：**无仓库搜索、无单包升级、无源管理**——UniGetUI 主卖点全缺。
- `needs_admin` 项只显示状态，无"提权重试"动作；托盘无 CPU/内存负载显示（§3.12 明确要求，D-26 set_title 通路已在）。
- 监控无磁盘 IO/GPU/温度，历史 300 点不持久化不导出；无阈值告警。
- 无开机启动项视图（Autoruns 心智完全空缺）。

## 2. 子面板信息架构

| 子面板 | 类型 | 内容 |
|--------|------|------|
| 概览 | 概览 | CPU/内存/磁盘/网络四卡（现曲线）+ 当前 Top4 进程 + 告警流（阈值/清理/更新数）+ 托盘负载开关 |
| 进程 | 清单 | 列表（CPU/内存/磁盘 IO/网络 排序 + 搜索 + 树形分组）+ 右键：结束进程/打开文件位置/复制到剪切板/加自动化规则（sys.process 事件）|
| 启动与恢复 | 清单 | 启动项（注册表 Run + 启动文件夹 + 计划任务简表）+ 服务（running/stopped 枚举）+ 启用/禁用（HKCU 直接、HKLM 走提权 helper 边界）|
| 存储与清理 | 清单+编辑 | `sys_clean_targets` 目标管理页 + 扫描/执行（现状保留）+ **磁盘分析 treemap**（大目录色块下钻 + 取消）|
| 软件 | 清单 | 已装列表（现状）+ **在线搜索安装**（winget search→结果卡）+ 单包升级 + 可升级徽标计数 + 源管理卡（可用/不可用+修复指引）|
| 系统调整 | 清单+编辑 | catalog 43 项分组浏览树（分类/风险/是否需管理员/当前状态）+ **应用前 diff 预览**（旧值→新值）+ 批量方案（WinUtil 式"推荐套餐"预设勾选）+ 全部还原 + 审计时间线（导出保留）|
| 设置 | 设置 | 采样间隔、托盘负载显示、磁盘空间告警阈值（本战役亲历 C 盘 10054 满盘事故→此告警是血的教训）、历史持久化开关 |

## 3. 对标功能矩阵

| # | 功能 | 来源 | 现状 | 实施 |
|---|------|------|------|------|
| 1 | CPU/内存/磁盘/网络实时曲线 | NeoHtop/§3.12 | ✅ | 补磁盘 IO；GPU/温度平台受限如实 DeferredBadge |
| 2 | 进程列表/排序/搜索/结束 | NeoHtop/§3.12 | ❌ | 新后端（Toolhelp 快照/PSAPI，win-integration）+ 保护名单（§5-①）|
| 3 | 托盘显示负载 | §3.12 | ❌ | tray.set_title 定时刷新（D-26 通路）|
| 4 | 多包管理器统一抽象 | UniGetUI/§3.8 | ✅ | — |
| 5 | 仓库搜索+安装 | UniGetUI | ❌ | `sys_pkg_search` 新命令（winget search/scoop search 解析）|
| 6 | 单包升级/版本对比 | UniGetUI | ❌ | run_action 扩 upgrade 动作 + 当前/可用版本列 |
| 7 | 批量安装清单 | UniGetUI 批量 | ❌ | 勾选多包一次排队（复用 cmd_preview 确认流）|
| 8 | 系统修复批处理（DISM/SFC） | WinUtil | 部分（whitelisted run）| 调整页"系统修复"卡组 + 提权边界提示 |
| 9 | 策略批量+还原默认 | WinUtil | 部分（apply/rollback）| 目录浏览 + 预设方案 + 全部还原（§5-③）|
| 10 | 清理自定义目标 | BleachBit | ❌ | clean_targets 接 UI + 白名单纪律（不可扩任意路径——红线维持）|
| 11 | 磁盘空间树图 | TreeSize/SpaceSniffer | ❌ | walkdir 后台扫描 + treemap 画布（可取消）|
| 12 | 启动项管理 | Autoruns | ❌ | 枚举三类源 + HKCU/HKLM 提权边界（08-winops 文档同源纪律）|

## 4. 其他软件借鉴

- **WinUtil**：Tweaks 的"默认/推荐/自定义"三档按钮组与执行日志流——diff 预览 + 套餐预设照此形态；
- **NeoHtop**：进程树折叠 + 右键杀进程子树（"结束进程及子进程"）；
- **UniGetUI**：安装来源角标（winget/scoop/choco）+ 已装/可升级双色徽章；
- **Task Manager 详细信息页**：列自选 + 置顶小字状态——进程页列配置。

## 5. 拓展设计（常人未思）

1. **进程保护名单硬编码红线**：csrss/lsass/wininit/services/svchost(系统账户组) 拒绝结束（本地名单+以会话归属判定），负例测试——把"误杀"关进笼子才有资格给"结束进程"。
2. **磁盘告警→automation 事件**：空间低于阈值 publish `sys.disk_low`，用户可挂规则（自动清理/仅通知）——跨模块闭环，蓝本没有、WinUtil 没有。
3. **注册表 diff 预览**：apply 前展示每动作 旧值→新值 表（备份读回比对），verify 失败自动回滚已有（BAVR），把"敢不敢点"变成"看得懂再点"。
4. **一键还原全部本工具所做调整**：BackupStore 按 origin 聚合 → "撤销 NexusForge 的全部改动"（信任设计）。
5. **"我的电脑健康周报"**：本地聚合卡（可清理量/可升级数/调整漂移/磁盘趋势），只读数据全部本地，可一键转发 automation 通知。

## 6. 验收点

- 进程页：排序/搜索/结束（含保护名单负例）；treemap 扫描可取消；
- `sys_pkg_search` 结果卡→安装 cmd_preview 一致；单包升级路径；
- catalog 浏览 43 项全覆盖 + diff 预览 + needs_admin 重试走 helper；
- 托盘负载开关生效且 CPU 采样失败时静默降级（负例）；
- 全部新命令 permissions+capability 同步；启动项 HKLM 写路径必须经提权 helper（红线负例：capability 断言 webview 无任意注册表写命令）。
