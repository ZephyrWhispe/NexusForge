# Security Policy / 安全策略

## Supported Versions / 支持版本

| Version | Supported |
|---------|-----------|
| master 分支最新提交 | ✅ |
| 已发布的 tag/安装包 | 最近一个版本（仅安全修复） |
| 更早版本 | ❌ 请升级 |

## Reporting a Vulnerability / 报告漏洞

**请勿在公开 issue / 讨论 / PR 中披露安全漏洞。**

1. 通过 GitHub 私密安全建议（Security → Report a vulnerability）提交，或
2. 私信仓库所有者（见 [CODEOWNERS](./CODEOWNERS.md)）。

请尽量包含：

- 受影响的功能面与提交号（`git rev-parse HEAD`）
- 复现步骤或 PoC（最小化）
- 影响评估（机密性/完整性/可用性；是否可跨设备/跨权限触发）

## 处理时序

- **确认**：48 小时内回复收到。
- **评估与修复**：按严重度分级——
  - 密码学误用 / 提权 / 数据破坏（P0/P1）：目标 7 天内热修 + 回归测试。
  - 其余（P2/P3）：随下一批次整改。
- **披露**：修复发布后在 Release Notes 登记（经报告者同意可致谢）。

## 安全基线速查（评审自查）

本项目已收敛的安全纪律（新增代码评审时对照）：

- AEAD 会话：收发方向密钥分离（`derive_directional_keys`）+ 帧序号校验，禁止双向复用同 key
- 提权 helper 输入：只允许枚举/白名单（exec 参数模板、注册表键/服务/计划任务白名单），禁止透传任意字符串
- 相对路径：外部来源一律过段级校验（notes `norm_rel` / file-core `safe_rel_path`），拒绝 `..`、盘符前缀、根相对、ADS
- 前端渲染：`dangerouslySetInnerHTML` 仅允许 `src/components/MarkdownView.tsx`（DOMPurify 白名单净化）
- asset 协议 scope：具体媒体子目录，禁止 `$APPDATA/**` 整根授权
- 信任根文件（identity/paired/known_hosts）：不可解一律 fail-closed，绝不覆盖重生
