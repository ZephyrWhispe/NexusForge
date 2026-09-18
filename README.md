# NexusForge

Windows 桌面效率集成套件：宿主框架 + 13 个功能模块，本地优先、数据不出机。

- **P0**：剪切板中枢 · 截图 · OCR/翻译（v1 单引擎）
- **P1**：键鼠共享（KVM）· 密码库 · 文件传输 · 代理 · 桌面效率
- **P2**：终端（ConPTY/WSL/SSH/Docker）· 文本与 PDF · 笔记 · 系统管理（含 WinOps）
- **P3**：自动化引擎（WASM 插件）· 跨设备同步

## 技术栈

- 后端：Rust（workspace，17 个 crate）+ Tauri 2
- 前端：React 19 + TypeScript + Fluent UI 9 + Vite
- 存储：每模块独立 SQLite（FTS5），数据集中于 `{appData}/db/`
- 平台：仅 Windows 10 1809+（Win32 能力统一收敛在 `crates/win-integration`）

## 仓库结构

```
crates/
  host-core/          宿主：Module trait、事件总线、配置中心、错误体系
  win-integration/    全部 Win32/WinRT 实现（clipboard/OCR/ConPTY/DPAPI/…）
  clipboard-core/ screenshot-core/ ocr-core/          P0 模块
  kvm-core/ vault-core/ file-core/ proxy-core/ desktop-core/  P1 模块
  term-core/ editor-core/ notes-core/ sys-core/                P2 模块
  automation-core/ sync-core/                                  P3 模块
  nexusforge-helper/   提权辅助进程（WinOps HKLM 操作）
src-tauri/            Tauri 壳与 IPC 契约层
src/                  前端（工作台、模块面板、子窗口）
docs/                 设计 / 实施 / 审查 / 决策文档
```

## 开发

前置：Windows 10 1809+、Rust（stable，MSVC 工具链）、Node.js 20+。

```bash
# 安装前端依赖
npm install

# 开发模式（HMR + Tauri 窗口）
npm run tauri dev

# 前端类型检查 / 构建
npx tsc --noEmit
npm run build

# Rust 测试（全部 mock，无需真实系统调用；ConPTY 相关用例按桌面会话门控 ignore）
cargo test --workspace

# 桌面会话中人工验收 ConPTY 终端链路
cargo test -p win-integration --test conpty_acceptance -- --ignored
```

## 文档

- [docs/DESIGN.md](docs/DESIGN.md) — 设计方案（执行依据，含 §11 修订登记）
- [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md) — 实施总纲与细化文档索引
- [docs/DECISIONS.md](docs/DECISIONS.md) — 决策记录（D-01…，规范偏离的唯一追加入口）
- [docs/REVIEW-2026-09-18.md](docs/REVIEW-2026-09-18.md) — 全量审查报告与整改批次

## 许可

GPL-3.0-only，非商业。第三方依赖许可见 [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。
