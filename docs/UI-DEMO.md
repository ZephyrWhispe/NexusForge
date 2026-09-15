# NexusForge UI Demo 说明文档

> 版本：v1.0 ｜ 日期：2026-09-15 ｜ 对应方案：[DESIGN.md](../DESIGN.md) §2 / [IMPLEMENTATION.md](../IMPLEMENTATION.md)
> Demo 文件：[demo/index.html](../demo/index.html) —— **单文件、零依赖，双击即可在浏览器打开**（建议 Chrome/Edge 全屏 F11 体验）。

## 1. 目的与范围

本 Demo 是阶段一开发前的**交互与视觉原型**，用于在写正式代码前锁定：

1. 主工作台信息架构（蓝本 UI 构想方案的可视化落地）
2. Fluent 风格设计令牌（颜色/圆角/间距/动效），后续直接翻译为 Fluent UI React v9 主题
3. P0 模块（剪切板）的核心交互闭环，以及 P1/P2 模块的空状态文案
4. 剪切板快速面板、启动器、截图覆盖层三类系统级 UI 的形态

**不在范围内**：真实数据读写、后端 IPC、多窗口、多显示器（均为静态演示或模拟）。

## 2. Demo 覆盖的界面清单

| 界面 | 对应正式实现 | Demo 中的呈现 |
|------|--------------|---------------|
| 标题栏 + 全局工具栏 | impl/01 S7、DESIGN §3.2 全局工具栏 | 主题切换、三类快捷入口、全局搜索框 |
| 模块导航（13 模块） | DESIGN §3 模块导航栏 | 按 P0/P1/P2 分组；P0 模块带运行状态点 |
| 二级导航（剪切板） | impl/02 C8 | 分组筛选（全部/文本/代码/链接/敏感/文件）+ 视图开关 |
| 剪切板历史列表 | impl/02 C6–C8 | Mock 14 条真实感数据：置顶、加密条目、图片缩略、代码等宽字体、悬浮操作、进入动效 |
| 剪切板快速面板 | impl/02 C8 QuickPanel | `Ctrl+Shift+V` 呼出，数字键直选 |
| 启动器 | impl/05 D1–D2 | `Alt+Space` 呼出，输入过滤 |
| 截图选区覆盖层 | impl/03 P3 | `Ctrl+Shift+S` 全屏暗化 + 拖拽选区演示 |
| 设置中心 | impl/01 S6.1 schema 驱动 | 开关/步进器/快捷键行，对应 ClipboardConfig 字段 |
| 状态栏 | DESIGN §3.5 状态栏 | 模块状态点（含未启用警告色）、DB 状态、快捷键提示 |
| Toast 通知 | DESIGN §10 通知系统 | 操作反馈与"错误可操作"文案示范 |

## 3. 交互演示指南

| 操作 | 效果 |
|------|------|
| 左侧切换模块 | 剪切板为完整演示；其余模块展示**空状态设计**（impl 文档验收项之一） |
| 全局搜索输入 | 实时过滤列表（模拟 FTS5 行为） |
| 悬浮列表条目 | 出现 ⏎粘贴 / ☆置顶 / ✕删除 操作按钮 |
| 点击条目 | Toast 演示"回写窗口防循环"提示（对应 impl/02 C3 ③） |
| `Ctrl+Shift+V` / `Alt+Space` / `Ctrl+Shift+S` | 三个系统级面板；`Esc` 全部关闭 |
| 标题栏 ◐ | 深色/浅色主题切换（两套完整令牌） |
| 设置中心开关/步进器 | 即点即生效的表单交互（演示 schema 自动生成的目标形态） |

## 4. 设计令牌（→ 正式实现映射）

Demo 的 CSS 变量即未来 Fluent UI v9 主题的**唯一事实来源**：

| Demo 令牌 | 正式实现（Fluent UI v9） |
|-----------|--------------------------|
| `--bg / --bg-2 / --bg-3` 三层背景 | `colorNeutralBackground1–6` + Tauri Mica 材质窗口 |
| `--accent(-solid/-soft)` | `colorBrandBackground` / `colorBrandBackground2`（品牌色映射 Windows 强调色） |
| `--text / --text-2 / --text-3` | `colorNeutralForeground1–4` |
| `--ok / --warn / --err` | `colorPaletteGreen/Orange/Red` 语义槽 |
| 圆角 6/8/12px | Fluent `borderRadiusMedium/Large/XLarge` |
| 动效 `pop/rise/slidein`（≤200ms） | `motionCurveDecelerateMax`；实现时尊重系统"减少动态效果"（Demo 已内置 `prefers-reduced-motion`） |

## 5. 关键设计决策（与 impl 文档对应）

1. **列表卡片化最小化**：条目用分隔线 + 悬浮高亮，不用厚卡片（frontend 规约 + Fluent 密度要求）。
2. **敏感条目展示**：预览文本固定为 `[敏感内容 · 类型]`，绿色→琥珀色边框 chip，杜绝明文泄露（impl/02 C4）。
3. **空状态即文档**：每个 P1/P2 模块空状态写明"怎么用 + 参考哪个 impl 文件"，首次启动引导的降级形态（蓝本 §8.1）。
4. **快速面板交互**：`数字键 1–9 直选`为硬交互，与剪切板堆栈粘贴语义一致（DESIGN §4.1 堆栈）。
5. **状态栏模块点**：绿=Running、琥珀=Stopped/未启用、红=Error —— 对应 impl/01 S5 状态机。

## 6. 已知简化（不作为实现依据）

- 数据为 Mock，不含分页/虚拟列表真实实现（正式版用 TanStack Virtual，见 impl/02 C8）
- 截图选区为单屏演示，正式版每显示器一个覆盖窗口 + 物理像素坐标（impl/03 P3）
- 多窗口/贴图置顶/桌面挂件未包含在 Demo，形态已在 DESIGN §3/§7 定义
- 浏览器内无法演示 Mica 真实材质，仅用渐变近似

## 7. 下一步

1. 按本 Demo 锁定的令牌与布局，在阶段一 S1 建立 `src/` React 骨架时迁移为 Fluent UI v9 主题
2. 剪切板面板按 impl/02 C8 组件树拆分（`ClipboardPanel / EntryCard / ClipboardQuickPanel / store / ipc`）
3. 本 Demo 保留为验收对照物：阶段一出口评审时逐项核对交互一致性
