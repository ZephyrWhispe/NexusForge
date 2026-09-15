# NexusForge 前端开发实施计划（基于 UI Demo）

> 版本：v1.0 ｜ 日期：2026-09-15 ｜ 上游：[UI-DEMO.md](./UI-DEMO.md)（交互/视觉基准）、[IMPLEMENTATION.md](./IMPLEMENTATION.md)（S/C/P/O 步骤）、[DESIGN.md](./DESIGN.md)
> 本计划是前端正式开发的执行依据：任务分解、依赖顺序、交付物、验收标准均以此为准。

---

## 1. 目标与范围

将 [demo/index.html](../demo/index.html) 锁定的交互与视觉基准，落地为正式前端代码：

- **技术基线**：React 18 + TypeScript 5 + Vite 5 + Fluent UI React v9 + Zustand 4 + TanStack Virtual + Tauri 2 IPC
- **范围**：主工作台、剪切板 UI、截图/OCR 覆盖层 UI、设置中心、通知体系（P0 全量 + 其余模块空状态框架）
- **不在范围**：P1/P2 模块完整 UI（阶段二/三按各自 impl 文档执行，但空状态与导航框架在本计划内交付）

### 1.1 单页 Demo → 多窗口正式版的关键映射（架构决策）

| Demo 形态 | 正式实现 | 依据 |
|-----------|----------|------|
| 单 HTML 页面 | **Tauri 多窗口**：`main`（工作台）、`quickpanel`、`overlay-*`（截图/OCR 覆盖层）、`pin-*`（贴图） | impl/03 P3/P6 |
| 全局搜索框 | 主窗口工具栏组件，状态入 Zustand | — |
| `Esc` 关闭全部 | 每窗口独立监听；覆盖层窗口 `focusout` 自动隐藏 | impl/03 P3 |
| 数字键直选 | quickpanel 窗口独立键盘处理器 | impl/02 C8 |

## 2. 设计系统落地（U1 任务的输入）

从 demo CSS 令牌生成 `src/styles/theme.ts`（Fluent UI v9 `Theme` 对象），映射表见 UI-DEMO.md §4。规则：

1. 亮/暗两套 `webLightTheme`/`webDarkTheme` 的**品牌槽位覆写**（accent→Windows 强调色读取，见 U1-3）
2. 圆角/间距/字体使用 Fluent 原生 token，禁止自定义旁路（除 Mica 背景层）
3. 动效统一 `motionCurveDecelerateMax`，时长 ≤ 200ms，全局尊重 `prefers-reduced-motion`（demo 已验证此交互）
4. 所有演示型渐变/阴影仅在 `MicaBackdrop` 组件内实现，业务组件禁止引用

## 3. 前端目录结构（阶段一交付形态）

```
src/
├── main.tsx                     # 入口：按 URL query 分发窗口角色
├── windows/                     # 多窗口入口（Tauri label 一一对应）
│   ├── MainWorkbench.tsx        # label=main
│   ├── QuickPanel.tsx           # label=quickpanel
│   ├── ScreenshotOverlay.tsx    # label=overlay-shot
│   ├── OcrResultOverlay.tsx     # label=overlay-ocr
│   └── PinWindow.tsx            # label=pin-{id}
├── layout/                      # U1 产出
│   ├── TitleBar.tsx  Toolbar.tsx  StatusBar.tsx  ModuleNav.tsx  SubNav.tsx
│   └── MicaBackdrop.tsx
├── theme/theme.ts               # U1 产出：demo 令牌 → Fluent Theme
├── modules/
│   ├── clipboard/               # U3 产出（对齐 impl/02 C8）
│   │   ├── ClipboardPanel.tsx  EntryCard.tsx  ClipboardQuickPanel.tsx
│   │   ├── GroupFilter.tsx  SecretBadge.tsx
│   │   └── store.ts  ipc.ts
│   ├── screenshot/              # U4 产出
│   │   ├── SelectionLayer.tsx  AnnotationCanvas.tsx  Toolbar.tsx  PinView.tsx
│   │   └── store.ts  ipc.ts
│   ├── ocr/                     # U5 产出：OcrResultPanel.tsx EngineStatus.tsx
│   └── placeholder/GenericModule.tsx   # U7 产出：空状态组件
├── settings/                    # U6 产出：SchemaForm.tsx（JSON Schema → Fluent 控件）
├── components/  Toast.tsx  HotkeyHint.tsx  VirtualList.tsx  EmptyState.tsx
├── stores/  session.ts  notifications.ts
├── ipc/  client.ts（类型化 invoke 封装 + 事件订阅）  types.ts（AppError DTO）
└── styles/  theme.ts  tokens.css
```

## 4. 任务分解（WBS）

> 每个任务含：依赖、产出、验收。任务完成定义（DoD）：代码合入 + 单测通过 + Storybook/演示对照 demo 通过 + clippy/eslint 零告警。

### U1 设计系统与应用骨架（对齐 IMPLEMENTATION.md S1）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U1-1 | Vite + React + TS + Fluent UI v9 工程初始化，`MultiWindowPlugin` 骨架 | S1 | 工程可 `tauri dev` | 空主窗口 + HMR |
| U1-2 | `theme/theme.ts`：demo 令牌 → Fluent Theme（亮/暗） | U1-1 | 两套 Theme 对象 + Storybook 基线页 | 色板与 demo 目测一致；令牌无旁路 |
| U1-3 | Windows 强调色读取（IPC `host_system_accent`）注入品牌槽 | U1-2, S7 | theme 动态化 | 系统改强调色 → 重启后 UI 跟随 |
| U1-4 | `MicaBackdrop` + 窗口材质启用（`windows:true` Mica） | U1-1 | layout 组件 | Win11 真机 Mica 生效 |
| U1-5 | TitleBar / Toolbar / StatusBar / ModuleNav / SubNav 五个布局组件 | U1-2 | layout/ 全部 | 与 demo 布局逐像素对照（±2px） |

### U2 多窗口与状态基座（新增任务，Demo 单页无法覆盖）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U2-1 | `main.tsx` 窗口角色分发（`new URLSearchParams(location.search).get('w')`） | U1-1 | windows/ 骨架 | 4 类窗口可同时打开互不干扰 |
| U2-2 | `ipc/client.ts`：类型化 `invoke` + `onEvent` 泛型订阅（含 AppError DTO 反序列化） | S7 | ipc 层 | 错误 DTO 单测覆盖全部 kind |
| U2-3 | `stores/session.ts`：主题/活跃模块/窗口管理 Zustand；跨窗口同步走 `emit` | U2-2 | session store | 双窗口主题实时同步 |
| U2-4 | 全局快捷键 → 窗口呼出链路（host 发事件 → 对应窗口 show/focus） | U2-3, S6.2 | 快捷键接线 | Ctrl+Shift+V/Alt+Space/Ctrl+Shift+S 行为与 demo 一致 |

### U3 剪切板 UI（对齐 impl/02 C8，P0 核心）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U3-1 | `VirtualList`（TanStack Virtual）+ 分页拉取（`clipboard_search`） | U2-2 | 通用列表组件 | 5000 条滚动 60fps；无全量加载 |
| U3-2 | `EntryCard`：四类条目渲染（文本/代码等宽/图片懒加载/文件组）、加密遮蔽、chip 体系 | U3-1 | EntryCard | 与 demo 列表视觉一致；secret 永不明文（对照 C4） |
| U3-3 | `GroupFilter`/`SubNav` 联动 + 筛选状态入 store | U3-1 | 筛选链路 | 分组/置顶/搜索三条件组合正确 |
| U3-4 | 悬浮操作（⏎/☆/✕）→ `clipboard_paste/pin/delete` IPC；粘贴调用后 500ms 回写窗口不重复入列 | U3-2, S3 | 操作链路 | Toast 反馈文案与 demo 一致 |
| U3-5 | `ClipboardQuickPanel` 独立窗口：数字键 1–9 直选、Enter 粘贴、自动失焦隐藏 | U3-4, U2-4 | quickpanel 窗口 | 呼出→消失全程键盘可达 |
| U3-6 | `clipboard.captured` 事件 → 列表头部插入 + rise 动效（合并 300ms 去抖） | U3-1, S4 | 实时更新 | 连续复制 10 次 UI 无卡顿、顺序正确 |

### U4 截图 UI（对齐 impl/03 P3–P6、P8）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U4-1 | `SelectionLayer`：多显示器窗口创建、物理像素选区、放大镜、Esc/Enter/双击语义 | U2-1, P2 | overlay-shot 窗口 | 150% DPI 下选区准确（对照 P3 验收） |
| U4-2 | `AnnotationCanvas`：8 工具状态机 + undo/redo（与 Rust `AnnotationSession` 同构的归一化坐标） | U4-1 | 标注层 | 与 Rust 侧导出图一致性抽样比对 |
| U4-3 | 标注工具条 + 文本输入 + 数字序号工具 | U4-2 | 工具条组件 | 键盘可完整操作（无障碍） |
| U4-4 | `PinView`：缩放/透明度/拖动/双击关闭，pins.json 恢复渲染 | U4-2, P6 | pin 窗口 | 重启恢复与 demo 交互一致 |
| U4-5 | 录制控制条（开始/停止/丢帧提示） | U4-1, P7 | 录制 UI | 停止后无残留状态 |

### U5 OCR UI（对齐 impl/04 O7）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U5-1 | `OcrResultOverlay`：跟随选区、文本可选中复制、"复制全部"（走回写窗口） | U2-1, O7 | overlay-ocr | 引擎降级提示条可见 |
| U5-2 | 翻译栏独立错误态 + 引擎状态页（`ocr_engine_status`） | U5-1, O8 | EngineStatus | 语言包缺失时给出系统设置跳转 |

### U6 设置中心（对齐 impl/01 S6.1）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U6-1 | `SchemaForm`：JSON Schema → Fluent 控件（toggle/stepper/text/select/keyseq） | U2-2, S6.1 | 通用表单引擎 | 剪切板 schema 渲染与 demo 设置页一致 |
| U6-2 | 快捷键编辑行：捕获录入 + 冲突检测结果展示（HOST_HOTKEY_001） | U6-1, S6.2 | HotkeyEditor | 冲突时展示可操作 hint |
| U6-3 | 配置读写接线：`host_config_get/set` + 保存即校验反馈 | U6-1 | 设置链路 | 非法输入被 schema 拦截并有提示 |

### U7 通知与空状态体系（对齐 DESIGN §10、蓝本 §8）

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U7-1 | `Toast`（内嵌）+ Windows Toast 双通道：按窗口焦点选择通道 | U2-2 | notifications store | 焦点内用内嵌、失焦走系统 Toast |
| U7-2 | `EmptyState` + 13 模块空状态文案库（demo 已定稿文案） | U1-5 | GenericModule | 文案与 demo 逐字一致 |
| U7-3 | 模块 Error 态 UI：状态栏红点 + "点击重启"（`host_module_restart`） | U7-1, S5 | 状态栏增强 | mock panic 场景可从 UI 重启 |

### U8 测试与视觉回归

| # | 任务 | 依赖 | 产出 | 验收 |
|---|------|------|------|------|
| U8-1 | Vitest + Testing Library：store/ipc 层单测 | U2-2 | 测试基座 | ipc DTO、筛选逻辑覆盖 ≥ 85% |
| U8-2 | Playwright + Tauri WebDriver：demo 对照截图（亮/暗 × 5 关键视图） | U1–U7 | 视觉基线 | 与 demo 布局差异人工评审归零 |
| U8-3 | 键盘可达性走查：全部面板无鼠标可完成（对照蓝本 §12 无障碍） | U3–U6 | 无障碍清单 | Tab 顺序/焦点环/ARIA 通过 |

## 5. 执行顺序与依赖图

```
U1-1 ──► U1-2 ──► U1-5 ──────────────┐
  │        └─► U1-3 ─┐               │
  └─► U1-4 ──────────┤               ▼
U2-1 ──► U2-2 ──► U2-3 ──► U2-4 ──► 【骨架可演示】
                                     │
        ┌────────────────────────────┼──────────────────┐
        ▼                            ▼                  ▼
     U3-1..U3-6（剪切板）        U6-1..U6-3（设置）    U7-1..U7-3（通知/空状态）
        │ U3 出口                    │                  │
        ▼                            └────────┬─────────┘
     U4-1..U4-5（截图 UI）                    ▼
     U5-1..U5-2（OCR UI）                U8-1..U8-3（测试收口）
```

- **U1 → U2 →（U3 ∥ U6 ∥ U7）→（U4 ∥ U5）→ U8**
- U4/U5 各自后端依赖（P2 捕获、O4 管线）由后端线并行交付；前端任务不阻塞等待，先用 IPC mock（`ipc/client.ts` 预留 `mock:` 前缀开关）。

## 6. 里程碑

| 里程碑 | 包含 | 出口标准 |
|--------|------|----------|
| M1 骨架可演示 | U1 + U2 | 主窗口五布局组件就位、多窗口可开、主题双套、快捷键链路通 |
| M2 剪切板闭环 | U3 + U6 + U7 | 复制→历史→搜索→粘贴全链路真数据；设置实时生效 |
| M3 覆盖层完成 | U4 + U5 | 截图选区→标注→贴图、OCR 结果面板全部真数据 |
| M4 验收收口 | U8 | demo 对照归零、无障碍走查通过、CI 全绿 |

## 7. 风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| Fluent UI v9 组件密度与 demo 有差（Table/List 圆角行为） | 布局走样 | U1-5 逐像素对照提前暴露；必要时用 `mergeClasses` 覆写而不绕过 token |
| 多窗口状态同步复杂（Zustand 单窗口实例） | 数据不一致 | 统一规则：状态归属窗口本地；跨窗口只同步"动作事件"，数据各自向 Rust 拉 |
| 覆盖层窗口闪烁/延迟 | 呼出体验 | 沿用 impl/03 P3 预热方案；Playwright 计时纳入 U8-2 |
| 图片缩略图（blob 懒加载）闪烁 | 视觉抖动 | EntryCard 固定尺寸占位 + 渐进解码；列入 U3-2 验收 |
| WebView2 缓存导致主题切换不彻底 | 双主题残留 | 主题状态入 session store 广播，CSS 变量驱动（demo 已验证该模式） |

## 8. 与后端接口的契约冻结点

- M1 前：冻结 `AppError` DTO 形状、事件信封 `{topic,source,payload,ts}`（impl/01 S2/S4）
- M2 前：冻结剪切板全部 IPC 签名（impl/02 C7）——此后前端不再等待后端字段变更
- 契约变更必须走 `docs/DESIGN.md` §6 修订，禁止前端侧 ad-hoc 兼容层
