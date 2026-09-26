# 10 构件与分流规则自动更新治理（B9 · 码级任务书）

> 地位：`docs/DECISIONS.md` **D-32**（预留，动工前按 D-22 规则 4 正式登记五要素，证据=本文件+ [`docs/panels/2026-09-19/92-research-final-audit-updates.md`](../panels/2026-09-19/92-research-final-audit-updates.md)）的唯一实施方案文档（plan-doc-first）。
> 用户直令（2026-09-19 五审）："看重的核心一定要设置好自动更新，还有分流规则也要自动更新，其他的一些核心类的都可以选择自动更新或者手动下载之类的功能。"
> 粒度：本文件按 09 §2.5 六栏任务书标准撰写，是"文档做到代码级"的样板批。
> 借鉴许可：软件不商用、仅个人使用（用户裁定）——内核更新/geo 刷新/订阅刷新机制可直读移植 v2rayN、Clash Verge Rev、OpenClash、xray install-script（GPL 系），见台账 92 §1/§2。

## 1. 现状盘点（全部源码核实，2026-09-19）

| 既有件 | 锚点 | 事实 |
|--------|------|------|
| Sidecar 下载/安装 | `crates/proxy-core/src/sidecar.rs:15-17` `DEFAULT_SINGBOX_VERSION="1.10.7"`、`DEFAULT_WINTUN_VERSION="0.14.1"`；`:20-28` 官方 GitHub Release/wintun.net 直链模板；`:33-40` `Manifest{kernel_id,kernel_version,sha256,installed_at,channel}` 落 `{appData}/proxy/bin/manifest.json`；`:56` `sha256_hex`；`:64+` `install_singbox_from_zip`（zip crate，CRC 校验，取包内 `sing-box.exe`） | **手动安装已存在**：`ProxyService::kernel_install(version: Option<String>)`（service.rs:194）→ `http_get`（reqwest+rustls，UA 固定，30s 超时，重试≤3）→ 解压装核 → `publish_state()`。无"有没有新版"概念——版本是编译期常量或用户手输。 |
| wintun | service.rs:205 `wintun_install()`（固定版本，无更新面） | 同上，手动、无检查 |
| 内核进程管理 | kernel.rs:40-47 `trait KernelDriver{id/exe_path/start(cfg,on_exit)}`；:49 `KernelHandle{stop/alive/health_check(inbound_port)/logs_snapshot}`；:105 `spawn_kernel` | 停核/起核/健康检查原语**已在**——B9 的回滚时序有现成积木 |
| 订阅解析 | sub.rs:54 `parse_subscription(content, sub_id) -> Vec<Node>`、:94 `parse_share_uri` | 只解析节点链接集合；**未读 HTTP 响应头**（`profile-update`/`profile-expires` 是订阅标准头），**无任何定时刷新**——"分流规则自动更新"两条轨（订阅轨+geo 数据轨）当前都是零 |
| 直连规则 | service.rs:332-347 `direct_rules()/set_direct_rules()` 存 `{appData}/proxy/rules.json` | 用户手编直连域列表，无更新面（保持手动，本批不动） |
| 周期任务先例 | sync-core/src/module.rs:384 `tokio::spawn` 心跳循环、:438 accept_loop（cancel token 收尾） | 宿主级调度循环的成熟形态可照搬 |
| 配置面 | host-core/src/config.rs:43 `ConfigStore{get_module/set_module/register_schema/schema_of}`；`:272 global_schema()` | 策略开关落 `get_module("artifacts")`，schema 在模块 init 注册 |
| 应用本体更新 | tauri-plugin-updater（Rust 侧，D-28 已上线签名链，minisign pubkey+每目标 endpoint） | **与本批构件通道端点互不共享**（台账 92 注意 12）；其验签形态是 artifact 通道 v2 可选增强（sigil 见 §7-6） |
| 能力面 | src-tauri/capabilities/{main,quickpanel,overlay,pin,launcher,notebar}.json；permissions 17 域 TOML；security_config.rs 完整性兜底测试 | 新命令域 `artifact` 按 D-28 惯例：`permissions/artifact.toml` 一命令一 `allow-artifact-<kebab>`，**仅 main.json 广播**；aux 禁列表扩 `artifact-install/-rollback`（构件替换=供应链动作，quickpanel/overlay 等永不可达） |

## 2. 架构决策

1. **新建 `crates/artifact-core`**（workspace 第 19 个 crate，host 直属、零业务模块依赖）：所有"从远端拉一个版本化构件装到本地并管好它"的共性收在这，代理/OCR/B2 新内核都只是注册表里的行。依赖：`reqwest`（workspace 已有 rustls 形态）、`sha2`、`zip`、`tokio`、`serde`、`thiserror`——全部现成，零新外部依赖（遵守 impl/01 头部依赖纪律表）。
2. **不引入新数据库**：状态文件学 proxy 的 JSON+manifest 先例——`{appData}/artifacts/state/{artifact_id}.json`（每构件的安装态，含 TOFU pin 历史）+ `{appData}/artifacts/update_log.json`（500 条环形，kernel.rs LOG_CAP 同款）。构件安装态天然按目录隔离，sqlite 表是多余的迁移面。
3. **检查与安装分离**：`Checker`（网络读：release 元数据/HEAD ETag）与 `Installer`（下载→staging→校验→健康→换装→回滚）两个结构体，auto 轨=Checker+（空闲窗口内）Installer；手动轨=UI 显式驱动。理由：检查永远轻量可高频，安装是有窗口期的重动作，二者节奏与失败面完全不同。
4. **分流规则自动更新 = 双轨**（用户点名项拆两义）：
   - **订阅轨**：扩展 `parse_subscription` 所在拉取路径，读响应头 `profile-update`（小时数，0=不自动）入 `SubscriptionMeta`（sub.rs 新增 struct + subs.json 字段，serde default 兼容旧文件），宿主调度按每订阅 interval 到期拉取→解析→偏离度门禁→应用→活动记录。
   - **数据轨**：sing-box geoip.db/geosite.db 与 B2 后各内核的 geo/rule-set 文件作为普通 artifact 进注册表，auto 轨每日跟检；B2 落地后 mihomo 系内核**优先走内核原生 geo-auto-update/rule-provider interval**（mihomo 配置字段事实，台账 92 #5），应用轨只兜底非内核托管者——两条实现先写进 B2 §5.5 小节防遗忘，本批只管 sing-box 时代的应用轨。
5. **依赖与批次序**：B9 **不依赖 B2** 即可交付 sing-box/wintun/geo 三件套+框架；B2 内核注册表落地时以"注册表加行+KernelDriver stop/start 钩子复用"接入（KernelDriver 已带 exe_path/stop/health 原语），届时新增 `KernelArtifactHook` trait 桥接——B9a 先行、B9b 随 B2 收编，两批可分提交。
6. **空闲窗口**（auto 只在不打扰时动手）：`installer_idle_ok()` = 内核 `!alive()` 或 当前时间在 `[03:00,05:00)` 本地窗且距上次自动替换≥24h；不满足→检查照跑，替换挂起为"待安装"徽章，下次窗口兑现。手动轨不受窗口限制但运行中换核走 D-18 ConfirmDialog（明示"将中断代理 N 秒"）。

## 3. 构件注册表（首批字面行；代码即 `artifact-core/src/registry.rs` 常量数组）

```rust
pub struct ArtifactSpec {
    pub id: &'static str,           // 稳定 wire id，进 IPC/状态文件名/日志
    pub label: &'static str,        // UI 显示名（constants 惯例）
    pub kind: ArtifactKind,         // KernelBinary | Runtime | GeoData | Model | ScriptPack
    pub source: ArtifactSource,     // 见 §4.1
    pub verify: VerifyMode,         // 见 §4.2
    pub default_policy: UpdatePolicy, // Off | Auto | Manual（三态，用户裁定形态）
    pub install: InstallTarget,     // 相对 appData 的安装目标 + zip 成员选择器
    pub size_hint_kb: u64,          // 磁盘预检三倍系数基数 + UI 体积展示
    pub auto_min_kb: u64,           // ≥此体积的 auto 需用户显式开启（模型类门槛）
}
```

| id | kind | source | verify | default_policy | install | 备注 |
|----|------|--------|--------|----------------|---------|------|
| `proxy.kernel.sing-box` | KernelBinary | GitHubRelease `SagerNet/sing-box`，资产模板 `sing-box-{v}-windows-amd64.zip` | 官方 `checksums.txt`（聚合文件，动工时以当期 release 资产实测锚定）失败回落 TOFU pin | **Auto** | `{appData}/proxy/bin/sing-box.exe`（zip 内单层目录成员，现 install_singbox_from_zip 逻辑搬迁复用） | 用户"看重的核心"首行 |
| `proxy.runtime.wintun` | Runtime | UrlTemplate：wintun.net 官方直链（sidecar.rs:27 现址，不换成第三方 GitHub 镜像——官方页即信任面最小说明） | TOFU pin（wintun 无校验文件发布面，如实登记） | Manual | `bin/wintun.dll` | 更新极罕见，默认手动下载；检查=版本号比对 0.14.1 |
| `proxy.geoip.db` / `proxy.geosite.db` | GeoData | GitHubRelease `SagerNet/sing-geoip` / `SagerNet/sing-geosite` 资产 `*geoip-{date}.db`（sing-box 官方文档约定的数据仓） | 同上双模 | **Auto** | `{appData}/proxy/geo/geoip.db`（新目录；B2 配置 IR 的 geo 路径字段引用此处） | 换名约定（date 版本）→ 状态文件记 `remote_version` |
| `ocr.lang.chi_sim`（示例行） | Model | Tesseract 官方数据仓 GitHub `tesseract-ocr/tessdata_fast` | TOFU pin | Manual + `auto_min_kb` 门槛 | ocr 数据目录 | B4 前只登记不接线；证明 Model 形态与"大构件默认手动"裁定（台账 92 #7）有落点 |
| （B2 占位）`proxy.kernel.xray` / `proxy.kernel.mihomo` 及各自 geo | — | — | — | — | — | 注册表加行即接入，禁止为内核写特例更新器 |

注册表纪律：`registry.rs` 有单元测试 `registry_ids_unique_and_paths_escape_free`——id 唯一 + install 相对路径无 `..`/绝对路径（zip-slip 同源防护伸到静态面）。

## 4. 数据结构与接口（字面签名，实现照抄级）

### 4.1 来源抽象

```rust
pub enum ArtifactSource {
    GitHubRelease { repo: &'static str, asset_match: fn(&str) -> bool, checksum_asset: Option<&'static str> },
    UrlTemplate { url: fn(&str) -> String, checksum_url: Option<fn(&str) -> String> }, // wintun 用
}
pub struct RemoteVersion { pub version: String, pub asset_url: String, pub checksum: Option<String>, pub fetched_at: u64 }
```

GitHub 元数据拉 `https://api.github.com/repos/{repo}/releases/latest`，带 `If-None-Match`（ETag 存状态文件，304 零流量）；403/429（限流）→ 回落 `https://github.com/{repo}/releases.atom`（HTML/XML，无 API 配额）；两者都败 → 本次检查记 `check_unreachable`，退避加倍（§6）。镜像：`GlobalConfig` 增 `artifacts.mirrors: Vec<String>`（默认空）——仅对下载 URL 生效，替换规则 `https://github.com/…` 前缀替换为镜像项，**校验文件永远从官方 checksum_asset 另取**（镜像只搬字节）。

### 4.2 校验与状态文件

```rust
pub enum VerifyMode { OfficialFileOrTofu, TofuPin }
pub struct ArtifactState {           // {appData}/artifacts/state/{id}.json（tmp+rename 写，manifest 同款）
    pub id: String, pub installed_version: String, pub sha256: String,
    pub pinned_sha256: Option<String>,   // TOFU 首装人工核对后 pin（null=未 pin，auto 轨对未 pin 构件只提醒不装）
    pub etag: Option<String>, pub last_check_ok: u64, pub last_check_err: Option<String>,
    pub next_check_after: u64,           // 退避闸门
    pub pending_version: Option<String>, // 空闲窗口未到而挂起的"待安装"
    pub bak_kept: bool,                  // 是否留有 {file}.bak（回滚弹药）
}
pub enum UpdatePolicy { Off, Manual, Auto }        // 存 ConfigStore 模块 "artifacts"：policies: BTreeMap<String,UpdatePolicy>，缺省取 spec.default_policy
pub struct ArtifactStatusDto { pub id: String, pub label: String, pub installed: String, pub latest: Option<String>,
    pub policy: UpdatePolicy, pub needs_update: bool, pub pending: bool, pub pinned: bool, pub last_result: Option<LogEntryDto> }
```

### 4.3 安装状态机（`installer.rs`，每步纯函数可测）

```rust
pub async fn install(spec: &ArtifactSpec, remote: &RemoteVersion, ctx: &InstallCtx, hooks: &dyn KernelHook) -> Result<InstallOutcome, ArtifactError>;
pub enum InstallOutcome { Installed { from: String, to: String }, DeferredIdle { to: String }, Rejected { reason: RejectReason } }
pub enum RejectReason { ChecksumMismatch, DowngradeWithoutConfirm, DiskShort(u64,u64), ZipSlip(String), Unarchive(String), HealthFail, MirrorPolicy }
pub trait KernelHook { fn kernel_running(&self) -> bool; fn stop_kernel(&self); fn start_kernel(&self) -> Result<(),String>; }
```

时序（KernelBinary；DataFile 无 3/6/7 的进程段）：
1. 磁盘预检：`剩余 ≥ size_hint_kb*3`（构件+解压峰值+旧 bak），不足 → `DiskShort`。
2. 下载到 `{appData}/artifacts/tmp/{id}-{version}.zip`，断线整包重来（构件都 <50MB 量级，v1 不做断点）。
3. 校验：OfficialFile 模式拉官方 checksum 文件比对；`ChecksumMismatch` → **删除下载物 + 记 error + 通知**（镜像投毒面在这关门）。TOFU 未 pin 时 auto 轨只产生"有新版本"通知，下载仅手动。
4. staging 解压：`tmp/{id}-{v}/`，逐条目路径规范化，成员名含 `..` 或绝对盘符 → `ZipSlip` 拒整包（负例测试字面名 `zip_slip_sample_rejected`，构造含 `..\..\evil.exe` 条目的内存 zip）。
5. 预检健康（换装前验新核）：staged 路径跑 `sing-box version`，输出含版本行才算过，失败 → `HealthFail` 丢弃 staging，**现役不动**。
6. 换装窗口判定：`hooks.kernel_running()` 且非空闲窗 → `DeferredIdle`（写 pending_version，UI"待安装"徽章）；手动轨已弹 D-18 确认后 `stop_kernel()`。
7. 原子换：旧文件 rename 为 `{name}.bak`（同目录同卷，替换成功前不删）→ staged 二进制+manifest+状态文件依次 move 到位 → `start_kernel()`（若换装前在运行）→ `health_check(mixed_port)` 10s 内过 = 成功。
8. 失败回滚：start 或 health 败 → 还原 `.bak`、重写旧 manifest、再 start、记 `RolledBack` 事件；`.bak` 保留一份（新装成功则旧 `.bak` 滚动替换为刚退役版本）。
9. 全程 `ArtifactEvent { id, phase, outcome, from, to, ts }` 上总线（事件名 `artifact.event`，落 update_log.json）+ 活动页。

`install_from_zip` 现逻辑从 proxy-core `sidecar.rs` **平移**进 artifact-core（proxy 侧改薄调用，保 `ProxyService::kernel_install` 签名不破——它已被 `proxy_kernel_install` 命令与既有前端消费；行为差异仅新增"下载后校验"步骤）。

### 4.4 调度（`src-tauri/src/artifact_scheduler.rs`，sync module.rs:384 形态）

```rust
pub fn spawn_artifact_loops(svc: Arc<ArtifactService>, proxy: Arc<ProxyService>, cancel: CancellationToken);
```
- 单 task、60s tick（零成本），到点判定三件事：**构件日检**（`now ≥ next_check_after` 且 policy=Auto 且非计量网络 §4.5 且 jitter：每日 03:00±15min 随机一次全量 Check）、**订阅到期**（每 sub `last_fetched_at + interval_h*3600 ≤ now` 且 interval>0 → 拉取→解析→偏离门禁→应用）、**pending 兑现**（有 `pending_version` 且现值空闲 → 走 install 尾段）。
- 启动时补检：应用启动 5s 后对 `last_check_ok` 超 24h 的 Auto 构件跑一轮 Check（CVS"启动时过期检查"裁定）。
- 退避：检查败 `next_check_after = now + min(2^n*24h, 24h 封顶)`、成功复位；离线（reqwest 连接类错误）不算失败计数、直接下 tick 再试。
- 订阅偏离门禁：新解析节点集 vs 现役：`|Δnodes|/max(1,old) ≥ 0.5` 或订阅从有→全空 → 不自动应用，记 `SubDrift` 待人工（通知+活动页"订阅大变，点击查看差异"），30 天无人处理保持现役（宁旧勿毒）。
- 通知复用宿主通知中心（15-host-shell 既有设计）+ 深链 `module=proxy&panel=updates`；托盘徽章（D-26 set_title 通路）仅在"有待安装/检查失败"时挂 `⟳`，与 KVM/sys 文案互斥优先级表登记。

### 4.5 配置 schema（ConfigStore "artifacts"，init 注册）

`policies: map<id, "off"|"manual"|"auto">`、`mirrors: string[]`、`notify_on_update: bool(默认 true)`、`auto_require_unmetered: bool(默认 true)`。计量查询：win-integration 新增 `pub fn network_is_metered() -> bool`（`windows` crate `NetworkList::GetMeteredNetworkCost`，纯读，失败按非计量放行=可用性优先，注释写明该保守向）。

## 5. IPC 与能力面

新文件 `src-tauri/src/commands/artifact.rs`，注册进 generate_handler；`permissions/artifact.toml` 一命令一 permission，全部仅进 `capabilities/main.json`：

| command | 签名 | 语义 |
|---------|------|------|
| `artifact_list` | `() -> Vec<ArtifactStatusDto>` | 列表页数据 |
| `artifact_check` | `(id: String) -> ArtifactStatusDto` | 单项手动检查 |
| `artifact_check_all` | `() -> Vec<ArtifactStatusDto>` | 工具条"检查全部" |
| `artifact_install` | `(id: String, version: Option<String>, allow_downgrade: Option<bool>) -> InstallOutcomeDto` | 安装/更新/指定版本装（version 手输=**"手动下载之类的"兑现点**：任意版本含降级，降级必 allow_downgrade=true，UI 走 D-18 输入构件名确认） |
| `artifact_policy_set` | `(id: String, policy: UpdatePolicyDto) -> ()` | 三态策略，写 ConfigStore |
| `artifact_pin` | `(id: String, action: PinActionDto) -> ()` | pin 当前/清除 pin/手工 pin 指定 sha（高级区） |
| `artifact_rollback` | `(id: String) -> ()` | 有 `.bak` 才可点（否则 disabled+tooltip 原因，00§4-2） |
| `artifact_log_list` | `(limit: u16) -> Vec<LogEntryDto>` | 活动页 |
| `subscription_update_now` | `(sub_id: String) -> SubUpdateOutcomeDto`（proxy 域 commands，非 artifact 域） | 订阅轨手动刷新，自动轨共用同一函数体 |

红线：aux capability 禁列表扩两行（`allow-artifact-install`、`allow-artifact-rollback` 永不出现在 quickpanel/overlay/pin/launcher/notebar——供应链替换动作与"渲染外部像素的窗"完全不共域）；security_config.rs 完整性测试自动覆盖（命令已声明未授权即红），**新增专项** `aux_capabilities_never_grant_artifact_install`。

## 6. UI 落点（00-spec 纪律内）

- **宿主设置中心新增子面板「更新中心」**（15-host-shell §9）：五类子面板之"列表型"——工具条（检查全部 / 最近检查时间 ctx / 过滤：需更新·已停用）+ 每构件一行（行高 48：图标+名称+当前→最新（tabular）+ 策略三态下拉 S 档 + 主按钮随状态变 [更新][待安装][重试]）+ 底部 Footer 活动计数；危险动作（降级、清 pin）进行溢出菜单 `…`（00§3）。高级区（expander）：镜像列表、通知开关、非计量开关。
- **代理模块概览卡**（02-proxy）：内核卡右上状态点——`最新✓ / 可更新⟳(badge u) / 待安装(badge warn) / 检查失败(badge err)` + 订阅卡到期倒计时与 interval 配置位（订阅编辑弹窗加"自动刷新：跟随订阅头 / 关闭 / 每 N 天"三选，字段 `interval_override`）。
- 分流规则页（B2 交付）不新增更新 UI——数据轨更新统一在宿主更新中心，规则页只展示 geo 版本行（避免同一状态两处可编辑，README 配置归位纪律）。
- 预览样张 ⑲：宿主更新中心 + 代理订阅卡更新位（B9 实施提交前补入 preview/index.html，与实现同 PR 审）。

## 7. 安全红线（全含负例，测试名即验收）

1. **信任锚不随镜像漂移**：`mirror_url_serves_bad_bytes_rejected_by_official_checksum`（镜像返回内容 hash ≠ 官方 checksums.txt → RejectReason::ChecksumMismatch，现役文件 mtime 不变断言）。
2. **zip-slip**：`zip_slip_parent_traversal_entry_rejected`、`zip_slip_absolute_drive_entry_rejected`（构造内存 zip 双负例；正例=同 zip 内合法成员仍装成？否——整包拒，断言 staging 已清空）。
3. **降级**：`auto_track_never_downgrades`（remote.version < installed 时 auto 轨 outcome=Rejected{DowngradeWithoutConfirm}）；`manual_downgrade_requires_flag`（allow_downgrade=None → 拒）。
4. **回滚**：`health_fail_after_swap_restores_bak`（注入 start_kernel 失败闭包 → .bak 归位 + 状态文件 installed_version 回旧 + RolledBack 事件落日志）。
5. **ACL**：`aux_capabilities_never_grant_artifact_install`（Rust 构建期读 capabilities/*.toml 断言，含 aux 全窗遍历）。
6. **TOFU 纪律**：`unpinned_artifact_auto_track_only_notifies`（未 pin 且无官方校验文件 → auto 不自动下载）。预留 v2：若本仓为自制构件（GeoSQLite 转档等）发带 sig release，`VerifyMode` 加 `Minisign(pubkey)` 复用 D-28 的 `valid_minisign_pubkey` 形态。
7. **备份联动**：`tofu_pin_mismatch_after_restore_forces_reconfirm`（BackupStore 恢复后状态 pin 与现场 sha 不符 → needs_reconfirm 置位，auto 冻结至手动核对）。
8. **磁盘**：`disk_short_precheck_refuses_before_download`（tmp 目录剩余不足断言，网络 mock 零调用）。

## 8. 任务书（六栏：锚点/签名/数据/门禁/回归/完成判据）

| 任务 | 内容 | 主要锚点 | 回归（正反例字面名） |
|------|------|----------|----------------------|
| T-B9-1 | `crates/artifact-core` 骨架：spec/registry/state/VerifyMode + 状态文件读写 | §3/§4 字面类型 | `registry_ids_unique_and_paths_escape_free`；`state_roundtrip_tmp_rename_no_partial`（写中断模拟留 tmp 不毁旧） |
| T-B9-2 | Checker：releases/latest+ETag+atom 回落+退避表 | §4.1 | `etag_304_zero_traffic_updates_last_check_ok`；`api_403_falls_back_to_atom`；`failure_sets_next_check_after_backoff` |
| T-B9-3 | Installer 状态机全九步（含 KernelHook、sing-box 平移接线） | §4.3 | §7 之 2/3/4/7/8 五条 + `install_from_zip_migration_keeps_proxy_kernel_install_contract`（旧命令行为面回归） |
| T-B9-4 | 调度 task + 启动补检 + pending 兑现 + 订阅轨 interval/偏离门禁 | §4.4；sub.rs 新增 `SubscriptionMeta{interval_hours:Option<u32>, last_fetched_at, etag:Option<String>}` 与 subs.json serde default 兼容 | `profile_update_header_parsed_interval`；`missing_header_defaults_to_manual`；`sub_drift_half_population_change_defers_apply`（负例，现役节点集不变）；`cancel_token_stops_loop_symmetric`（D-19 生命周期对称） |
| T-B9-5 | 命令+权限+capability+win-integration `network_is_metered()` | §5/§4.5 | §7-5 + `metered_true_blocks_auto_but_manual_proceeds`（mock 注入）；`policy_set_roundtrips_config_store` |
| T-B9-6 | UI：宿主更新中心子面板 + 代理概览/订阅卡位 + schema 挂宿主设置表单 + preview ⑲ | §6 | vitest：`artifactPolicyOptions_match_threeStates`、`rollbackButton_disabled_without_bak_tooltip`；实启冒烟见 §9-A9 |

每任务一提交、提交带门禁全组——顺序即 1→2→3→5→4→6（能力先于调度，UI 最后与冒烟同批）。

## 9. 验收标准（机器可判定）

- A1 注册表含 sing-box/wintun/geoip/geosite 四行且 id 唯一；`cargo test -p artifact-core` 全绿。
- A2 断网启动应用：更新中心可见、无未处理 panic、日志仅 `check_unreachable`、下次 tick 自动重试（实启冒烟 15s 观察）。
- A3 对 sing-box：装 1.10.x 后 release 有更新 → auto 轨在下个日检点完成换装（或 DeferredIdle 后空闲兑现），update_log 有 Installed(from,to)；手动 `artifact_install(version=旧版本)` 无 allow_downgrade 被拒（CLI 级集成测试）。
- A4 投毒三负例（§7-1/2/6）全红转绿通过；`cargo test --workspace` 含 security_config 新断言。
- A5 内核换装中途杀进程（测试注入 panic 于 move 前）：重启后现役 sing-box.exe 完好、状态文件 installed_version=旧、可再次检查重装（崩溃原子性）。
- A6 订阅带 `profile-update: 72` 头：到期自动刷新并应用（diff 低于阈值），活动页两条记录；无该头订阅：永不自动刷新。
- A7 quickpanel/overlay 任一窗调 `artifact_install` → 拒且错误串点名窗与权限（D-28 CDP 同款实启验证）。
- A8 托盘/通知：自动更新成功与检查失败各产生一条宿主通知（总线可观测，clipboard D-26 同款"invoke 闭包→收事件"测试）。
- A9 实启冒烟走查：更新中心渲染真数据（manifest 现值）、策略切换持久化（重启后仍在）、"检查全部"节流不重复打网络（mock 层调用计数）。
- A10 门禁全组绿（fmt/clippy -D/test/bench --no-run/tsc/eslint/vitest/build）；preview ⑲ 与实现同提交。

## 10. 状态

`未开工`（等用户放行 D-29 系列审核）。依赖标注：B9a（本文全部，sing-box 时代）不阻塞于任何批；B9b（xray/mihomo/Pre-socks 内核与 rule-set 数据轨收编进注册表、mihomo 原生 geo-auto 配置面）随 B2 交付，届时在本文追加 §11 增量小节，不改已定接口。
