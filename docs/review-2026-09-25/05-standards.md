# 05 · 代码规范与工程治理问题详解

> 覆盖 `STD-01`…`STD-10`（代码规范）与 `GOV-01`…`GOV-09`（工程治理）。每条含：问题描述（含证据）→ 解决方案（含配置片段）→ 修复前后对比 → 预防措施。

---

## 一、代码规范

# STD-01 · 静默吞错的残留形态 + 三套错误解析实现　`P2` `[实测]`

### 位置
- [src/components/DryRunDialog.tsx:108](../../src/components/DryRunDialog.tsx#L108)、[src/modules/file/NameFixDialog.tsx:111](../../src/modules/file/NameFixDialog.tsx#L111)：
  `void props.onConfirm().catch(() => {}).finally(() => setBusy(false));`
- 私有错误解析：[src/settings/SchemaForm.tsx:209-213](../../src/settings/SchemaForm.tsx#L209-L213)（`parseErr`）、`src/windows/overlay/scrollFlow.ts:73-79`（`scrollErrorText`）、`src/windows/OverlayShot.tsx:378-385`（`fmtErr`）、`src/windows/LauncherWindow.tsx:93`（内联）
- 既有统一入口：[src/ipc/client.ts:15-26](../../src/ipc/client.ts#L15-L26)（`parseAppError`，含 `code`/`hint`/`retryable`）

### 问题描述
两处 `catch(() => {})` 把确认回调的 rejection 静默吞掉；而 ESLint 的 `no-empty`（`eslint.config.js:18` 已设 `allowEmptyCatch:false`）**不覆盖函数体 `{}`**，也不报含注释的块，因此这类写法**逃过了门禁**（前次报告 D-19 已要求清理裸 catch，这两处是漏网）。

同时错误解析散落三套私有实现，均**丢弃 `code` 与 `retryable`**（`SchemaForm.parseErr` 只拼 `message + hint`），使 `DESIGN.md §8.1` 的错误分级在前端无处消费。

### 影响
- 未来新增调用方若不自捕获，错误将**真丢失**（当前两个调用方各自捕获并 `setError`，属"潜伏"风险）。
- 错误文案与分级不统一；重试能力（`retryable`）无人消费，用户看不到"可重试"引导。

### 解决方案

**步骤 1：给对话框组件暴露 `onError`（或统一上报）**

```tsx
// src/components/DryRunDialog.tsx
interface Props { onConfirm: () => Promise<void>; onError?: (e: unknown) => void; /* ... */ }

const run = async () => {
  setBusy(true);
  try {
    await props.onConfirm();
  } catch (e) {
    (props.onError ?? ((err) => reportError(err, { context: "确认执行失败" })))(e);
  } finally {
    setBusy(false);
  }
};
```
（`NameFixDialog.tsx` 同改。）

**步骤 2：收敛错误文案到单一工具**

```ts
// src/ipc/errors.ts（新增，或并入 client.ts）
export function errorText(e: unknown, opts?: { withHint?: boolean }): string {
  const ae = parseAppError(e);
  if (!ae) return String(e);
  return opts?.withHint && ae.data.hint ? `${ae.data.message}（${ae.data.hint}）` : ae.data.message;
}
```
删除 `parseErr`/`scrollErrorText`/`fmtErr`，改调 `errorText(e, { withHint: true })`。

**步骤 3：补门禁（让这类写法变红灯）**

```js
// eslint.config.js —— 追加
{
  rules: {
    "no-empty-function": ["error", { allow: ["arrowFunctions"] }],   // 允许 () => {} 仅作为占位需显式注释豁免
    "@typescript-eslint/no-floating-promises": "error",
    "no-restricted-syntax": ["error",
      { selector: "CallExpression[callee.property.name='catch'] > ArrowFunctionExpression[body.body.length=0]",
        message: "禁止空 catch：请使用 reportError 上报或显式忽略并注释原因" },
    ],
  },
}
```
（若 `no-empty-function` 误伤合法占位，可按目录 override，但需在 PR 中说明。）

**步骤 4：测试** —— 为两个对话框加"确认回调失败时上报错误"的用例：
```tsx
it("确认失败时上报错误且不静默", async () => {
  const spy = vi.spyOn(notifyStore, "reportError");
  render(<DryRunDialog onConfirm={() => Promise.reject(new Error("boom"))} ... />);
  await userEvent.click(screen.getByRole("button", { name: "执行" }));
  expect(spy).toHaveBeenCalled();
});
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 空 catch | 2 处 `.catch(() => {})` | 0（统一 `reportError`） |
| 错误解析 | 4 套实现，丢 `code`/`retryable` | 1 个 `errorText` + `parseAppError` |
| Lint 覆盖 | `no-empty` 覆盖不到函数体 | 自定义 `no-restricted-syntax` 命中 |
| 测试 | 无 | 2 条行为断言 |

### 预防措施
1. **把"静默吞错"从习惯问题变成门禁问题**（自定义 ESLint selector + `no-floating-promises`），比人工 review 可靠。
2. **错误处理单一入口**：所有用户可见错误经 `reportError`；所有文案经 `errorText`；禁止在业务组件里自行解析错误对象。
3. **定期量化**：把 `catch` 形态纳入审查脚本度量（本次测得：严格空 catch 0 处、`.catch(()=>{})` 2 处、`.catch(()=>undefined)` 型 6 处、含注释的空 catch 4 处），作为"静默吞错"的持续指标。

---

# STD-02 · JSX 字符串反斜杠未转义（placeholder 显示错乱）　`P2` `[实测]`

### 位置
- [src/modules/kvm/KvmPanel.tsx:736](../../src/modules/kvm/KvmPanel.tsx#L736)：`placeholder="例如 C:\Users\me\Downloads\report.pdf"`
- [src/modules/automation/RulesPanel.tsx:867](../../src/modules/automation/RulesPanel.tsx#L867)：`placeholder="https:// 或 C:\path"`
- [src/modules/automation/RulesPanel.tsx:1094](../../src/modules/automation/RulesPanel.tsx#L1094)：`placeholder="插件目录路径，如 D:\plugins\demo"`

### 问题描述
JSX 属性值里的 `\U`、`\D`、`\p` 不经 JS 字符串转义规则处理时会保留（`\p`/`\D` 不是合法转义 → 保留反斜杠），但 `\r` 是**合法转义**（回车）→ `C:\Users` 变成 `C:` + CR + `sers`，显示错乱、可能吞字符并插入控制符。正确写法（同仓已有）：[DesktopPanel.tsx:561](../../src/modules/desktop/DesktopPanel.tsx#L561)、[EditorPanel.tsx:613](../../src/modules/editor/EditorPanel.tsx#L613)、[ConnectDialog.tsx:176](../../src/modules/file/ConnectDialog.tsx#L176) 均用 `\\`。

### 影响
用户看到的示例路径被吞字符/含控制符（如 `C:\Users\me\Downloads\report.pdf` 显示成 `C:UsersmeDownloadsreport.pdf`），指引失效（这类示例正是"复制粘贴到输入框"的依据）。

### 解决方案
三处统一为双反斜杠，或改用模板字符串与 `String.raw`：
```tsx
placeholder={String.raw`例如 C:\Users\me\Downloads\report.pdf`}
placeholder={String.raw`https:// 或 C:\path`}
placeholder={String.raw`插件目录路径，如 D:\plugins\demo`}
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 显示 | 吞字符/含 CR 控制符 | 与源码一致 |
| 一致性 | 同仓 3 处正确、3 处错误 | 全部统一 |

### 预防措施
在 `eslint.config.js` 启用 `react/no-unescaped-entities` 之外，另加 `no-useless-escape` 与自定义规则禁止 JSX 属性字符串中出现单反斜杠路径（可用 `no-restricted-syntax` 匹配 `Literal[value=/\\[A-Za-z]/]`）；review 时对"路径示例"统一用 `String.raw`。

---

# STD-03 · 可访问性残留：icon-only 按钮无名称、`<span role="button">` 嵌在 `<button>` 内　`P2` `[实测]`

### 位置
- [src/modules/vault/VaultPanel.tsx:871](../../src/modules/vault/VaultPanel.tsx#L871)（`icon={<AddRegular />}` 无 `aria-label`）、[:899-904](../../src/modules/vault/VaultPanel.tsx#L899-L904)（删除条目）、[:982-989](../../src/modules/vault/VaultPanel.tsx#L982-L989)（移除字段）
- [src/windows/OverlayShot.tsx:1588,1591](../../src/windows/OverlayShot.tsx#L1588)（撤销/重做，无 `aria-label` 也无 `title`）
- [src/modules/term/TerminalPanel.tsx:856-877](../../src/modules/term/TerminalPanel.tsx#L856-L877)（会话胶囊：`<span role="button" tabIndex={0} onKeyDown=…>` **嵌套在 `<button>` 内**）

### 问题描述
前三类按钮无 children、无 `aria-label`，读屏只播报"按钮"；TerminalPanel 的关闭钮把交互元素嵌套在按钮内部（**HTML 非法**），Tab 焦点与 Enter 语义歧义，屏幕阅读器可能只报外层按钮。对照正确写法：[EditorPanel.tsx:645-664](../../src/modules/editor/EditorPanel.tsx#L645-L664)（容器 `div` + 两个真 `<button>`）。

### 影响
键盘/读屏用户无法辨识与操作（含凭据删除这类关键操作）；嵌套交互元素在某些浏览器/辅助技术下行为不一致。

### 解决方案
```tsx
// ① icon-only 补可访问名
<Button size="small" icon={<AddRegular />} aria-label="新建文件夹" onClick={() => void doAddFolder()} />
<Button appearance="subtle" size="small" icon={<DeleteRegular />}
        aria-label={`删除条目 ${entry.title}`} onClick={() => void doDeleteEntry(entry)} />
<Button size="small" icon={<UndoRegular />} aria-label="撤销" title="撤销" onClick={undo} />

// ② 会话胶囊改为"容器 + 两个真按钮"
<div className={styles.tab}>
  <button className={styles.tabMain} onClick={() => setActive(s.id)}>{s.title}</button>
  <button className={styles.tabClose} aria-label={`结束会话 ${s.title}`} onClick={() => void killSession(s)}>
    <DismissRegular />
  </button>
</div>
```

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| icon-only 按钮 | 无名称 | `aria-label`（含对象名，如"删除条目 X"） |
| 嵌套交互元素 | `<span role=button>` 嵌 `<button>` | 容器 + 两个合法 `<button>` |
| 键盘可达 | 语义歧义 | Tab 顺序与 Enter/Space 明确 |
| ESLint | `jsx-a11y` 已启用但未覆盖该模式 | 补规则后变红灯 |

### 预防措施
1. `eslint.config.js` 的 `jsx-a11y` 增补规则：`jsx-a11y/control-has-associated-label`、`jsx-a11y/no-noninteractive-element-to-interactive-role`、`jsx-a11y/interactive-supports-focus`（后两条可命中"span 当按钮"与"无焦点能力"）。
2. 建立 `IconButton` 共享组件，把 `aria-label` 设为**必填 prop**（TS 类型层面强制），从根上消除"漏写名称"。
3. 把"键盘可达 + 可访问名"纳入 UI 验收清单（本次审查测得 `aria-label` 65 处、`aria-live` 4 处、`role=button` 12 处——相比前次已有明显改善，剩余为局部遗漏）。

---

# STD-04 · 超长文件与超长组件（单文件多子领域混居）　`P2` `[实测]`

### 位置与度量（实测行数）

| 模块 | 文件 | 行数 | 主组件体 |
|---|---|---|---|
| term | [TerminalPanel.tsx](../../src/modules/term/TerminalPanel.tsx) | 1307 | ≈1185（本地/WSL/SSH/SFTP/转发/Docker/exec 七块） |
| notes | [NotesPanel.tsx](../../src/modules/notes/NotesPanel.tsx) | 1284 | ≈1068（笔记库+双链+复习+画布） |
| automation | [RulesPanel.tsx](../../src/modules/automation/RulesPanel.tsx) | 1103 | ≈770 |
| vault | [VaultPanel.tsx](../../src/modules/vault/VaultPanel.tsx) | 1072 | ≈797 |
| sys | [SysPanel.tsx](../../src/modules/sys/SysPanel.tsx) | 1046 | ≈929（监控+进程+清理+包管+调整） |
| file | [FilePanel.tsx](../../src/modules/file/FilePanel.tsx) | 992 | ≈843 |
| clipboard | [HistorySection.tsx](../../src/modules/clipboard/panels/HistorySection.tsx) | 863 | ≈561 |
| editor | [EditorPanel.tsx](../../src/modules/editor/EditorPanel.tsx) | 770 | — |
| screenshot | [ScreenshotPanel.tsx](../../src/modules/screenshot/ScreenshotPanel.tsx) | 766 | ≈629 |
| kvm | [KvmPanel.tsx](../../src/modules/kvm/KvmPanel.tsx) | 741 | ≈673 |

Rust 侧同类问题前次已记：`KvmModule::start` 480 行、`sys-core/winops.rs` 1369 行；本次复核 `winops.rs` 仍为单文件多子领域。

### 问题描述
单文件承载多个子领域（如 `TerminalPanel` 同时含终端、SFTP、端口转发、Docker、远端执行），单个组件体近千行。这直接导致：改动风险高（一处修改变动多个关注点）、评审困难、测试难以定位（`__tests__` 也只能按行为切片）。

### 影响
- 缺陷密度与回归概率上升（本次多个正确性问题都落在这些大文件里：`COR-15`（NotesPanel）、`COR-16`（FilePanel/HistorySection）、`COR-18`（VaultPanel）、`COR-26`（layout）等）。
- 新成员理解成本高；同一文件的样式/状态互相耦合（如 TerminalPanel 的胶囊样式还重复了 `Tabs` 组件）。

### 解决方案（渐进式，不要求一次性重构）

**步骤 1：按"子领域 = 子组件 + 子 hook"切分**（保持行为不变，先搬代码）：
```
src/modules/term/
  TerminalPanel.tsx          (≈200 行：布局 + 子面板选择)
  sections/DockerSection.tsx
  sections/SftpSection.tsx
  sections/ForwardSection.tsx     （已存在）
  sections/ExecSection.tsx
  hooks/useTerminalSession.ts
  hooks/useSftpTransfer.ts
```
**步骤 2：把可测逻辑抽为纯模块**（如 `dib.ts`、`dragDropFlow.ts` 已是此模式，继续推广）：状态机/换算/校验移出组件 → 单测覆盖成本骤降。

**步骤 3：设"预算 + 渐进收敛"约束**：新增文件 ≤ 600 行、单组件 ≤ 400 行；对既有超标文件，**每次功能改动顺带拆分一块**（"Boy Scout" 规则），不新增大文件。

**步骤 4：CI 可见性**：加一个轻量脚本统计"超过阈值的文件数"，在 CI 输出（不阻断），作为趋势指标。

### 修复前后对比
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 单文件规模 | 700–1300 行 | 目标 ≤600 行 |
| 子领域耦合 | 同文件共享 state/样式 | 子组件独立 props/state |
| 测试定位 | 按行为切片、耦合 | 子组件/hook 单测 |
| 新增大文件 | 无约束 | 预算 + 顺带拆分 |

### 预防措施
把"文件/组件行数预算"写进 `CONTRIBUTING.md`（或 `UI-PLAN.md`），并明确"超标文件只减不增"；评审时对"往大文件里再加一块"提问"是否应先拆出子组件"。

---

# STD-05 · 前端量化债务：magic px / 内联 style / 硬编码颜色　`P3` `[实测]`

**度量**（非测试范围）：
- magic px 字符串字面量：**552 处 / 63 文件**（集中于 `OverlayShot` 50、`HistorySection` 28、`VaultPanel` 23、`NotesPanel` 23、`ScreenshotPanel` 23）；
- 内联 `style={{`：**168 处 / 28 文件**；
- 硬编码 `#hex`/`rgba()`：**98 处 / 10 文件**，其中 `src/theme/theme.ts` 57 处为**合法主题定义**，实际债务 ≈41 处（`OverlayShot` 20、`MicaBackdrop` 6、`PinWindow` 2 等）。

另有两处具体点：[term/ForwardSection.tsx:220](../../src/modules/term/ForwardSection.tsx#L220) `color:"#c50f1f"`、[term/TerminalPanel.tsx:114](../../src/modules/term/TerminalPanel.tsx#L114) `backgroundColor:"#1b1b1b"`。

**问题**：未走 Fluent token 体系 → 不随主题/高对比模式切换；数值散落 → 改版困难（前次报告 M2 同类问题，仍存在）。

**解决方案**
1. 颜色类：换用 token（如 `tokens.colorPaletteRedForeground1`、`tokens.colorNeutralBackground1`）；canvas/终端这类**必须有具体色值**的场景（xterm 主题、覆盖层绘制）集中为具名调色板常量：
```ts
// src/windows/overlay/OVERLAY_PALETTE.ts
export const OVERLAY_PALETTE = { stroke: "#e3008c", handle: "#ffffff", dim: "rgba(0,0,0,.45)" } as const;
```
2. 尺寸类：优先 `tokens.spacingHorizontalM` 等语义 token；对必须像素对齐的绘制逻辑，抽局部常量表（如 `const ROW_H = 32`）而非散落字面量。
3. 渐进执行：新代码不得新增字面量；改动旧代码时顺带替换（配合 `STD-04` 的"顺带拆分"）。

**前后对比**：修复前：主题切换不彻底、改版需全局搜索；修复后：颜色/尺寸集中可控，高对比模式跟随。

**预防措施**：加 ESLint 规则禁止在 `style` 中出现颜色字面量（可用 `no-restricted-syntax` 匹配 `Literal[value=/^#|rgba?\\(/]`），并把调色板常量目录列入 review 清单；度量值纳入审查脚本做趋势跟踪。

---

# STD-06 · Tab 胶囊样式仍有两处本地副本　`P3` `[实测]`

**位置**：[term/TerminalPanel.tsx:96-109](../../src/modules/term/TerminalPanel.tsx#L96-L109)、[editor/EditorPanel.tsx:71-105](../../src/modules/editor/EditorPanel.tsx#L71-L105) vs 已存在的共享组件 [components/Tabs.tsx:11-24](../../src/components/Tabs.tsx#L11-L24)（其注释正称"tab 样式此前 5 份重复，已统一"）。

**问题**：两处仍在各自 `makeStyles` 里定义 `tab/tabActive`（border/radius/背景近似），样式漂移风险（改 token 只改一处）。

**解决方案**：给 `Tabs` 增加插槽能力（`renderTab`/`trailing`）以承载"关闭钮/未保存标记"，两处改用它：
```tsx
<Tabs
  items={tabs}
  active={activeId}
  onChange={setActiveId}
  renderTab={(t, base) => <>{base}{t.dirty && <span aria-label="未保存">●</span>}</>}
  trailing={(t) => <button aria-label={`关闭 ${t.title}`} onClick={() => close(t.id)}>✕</button>}
/>
```
至少也应把胶囊样式抽为 exported `tabStyles` 共享。

**前后对比**：修复前 3 套近似实现；修复后 1 套 + 插槽。

**预防措施**：**新增 UI 片段前先检索 `src/components`**（本次审查还发现 `.section` 卡片、`.error/.ok` 等仍有多份副本）——把"先查共享组件"写进前端开发清单；对已抽出的共享组件，用 grep 断言"同类样式名不再出现在 modules 目录"。

---

# STD-07 · Toaster 运行时注入 `<style>`（CSP 例外点）　`P3` `[走查]`

**位置**：[src/components/Toaster.tsx:89](../../src/components/Toaster.tsx#L89)：`<style>{"@keyframes nf-toast-in{…}"}</style>`

**问题**：运行时注入 `<style>`，其生效依赖 CSP `style-src 'self' 'unsafe-inline'`；而项目为规避打包期 nonce 注入**已刻意清空 `index.html` 内联样式**（见 [src/\_\_tests\_\_/indexHtmlInlineStyle.test.ts:7-11](../../src/__tests__/indexHtmlInlineStyle.test.ts#L7-L11)、`src/styles/global.css:1-4`）。这是该 CSP 约束的一处例外点：若将来把 `style-src` 收紧为 nonce 制，toast 动画会**静默失效**。

**解决方案**：把 keyframes 移入 `src/styles/global.css`（链接 CSS 在打包期不受 nonce 影响），组件只保留类名。

**前后对比**：修复前依赖 `unsafe-inline`；修复后可收紧 CSP（配合 `SEC-14` 的 CSP 加固）。

**预防措施**：与 `SEC-14` 联动——把 `style-src 'unsafe-inline'` 的**唯一理由**限定为"Fluent UI 需要"，并在 `security_config.rs` 或前端测试中断言"不新增运行时 `<style>` 注入"（现有 `indexHtmlInlineStyle` 测试可扩展为扫描源码）。

---

# STD-08 · `ModulePlaceholder`/`placeholderFor` 死代码　`P3` `[实测]`

**位置**：[src/layout/panels.tsx:37-59](../../src/layout/panels.tsx#L37-L59)

**问题**：`ModulePlaceholder` 与 `export function placeholderFor(...)` 定义后**全库无使用者**（14 个模块均已有真实面板，`PANELS` 是穷尽 `Record<ModuleId,…>`）。

**解决方案**：删除这两段（若需保留占位能力，应在真正出现占位场景时再引入，并配测试）。同时清理其样式类（若有）。

**前后对比**：修复前死代码误导读者以为仍有占位路径；修复后注册表即唯一事实。

**预防措施**：定期用 `ts-prune`/`knip` 类工具扫描未使用导出（可纳入 CI 输出）；删除时同步清理相关样式与测试引用。

---

# STD-09 · `unsafe` 缺 `// SAFETY:` 说明与边界校验　`P3` `[实测]`

**位置**：[win-integration/src/dpapi.rs:22-45](../../crates/win-integration/src/dpapi.rs#L22-L45)（`protect`/`unprotect`：`slice::from_raw_parts(out.pbData, out.cbData)` + `LocalFree`，**无 SAFETY 注释、`pbData` 空指针/零长度未判**）；[win-integration/src/usn.rs:136](../../crates/win-integration/src/usn.rs#L136)（长度未校验）。

**问题**：前次报告 M5 要求"逐处补 SAFETY 并封装裸切片"，本次复核：`unsafe` 仍无 SAFETY 说明（全仓 `unsafe` 151 处中，绝大部分在 win-integration FFI 边界，说明覆盖率仍低）。

**解决方案**
1. 补 SAFETY 注释（说明指针来源、所有权、生命周期、长度保证）：
```rust
// SAFETY: DPAPI 成功返回时保证 pbData 指向 cbData 字节的可读缓冲，且所有权归本次调用；
// 我们在拷贝后立即 LocalFree，pbData 不会被再次使用。
let slice = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) };
```
2. 加空指针/零长度防护：
```rust
if out.pbData.is_null() || out.cbData == 0 { /* 视为空明文并返回明确错误 */ }
```
3. 把"裸切片 + 长度"封装为带校验的辅助函数（如 `unsafe fn slice_from_blob(ptr, len) -> Result<&[u8]>`），集中写一次 SAFETY 理由。

**前后对比**：修复前 `unsafe` 无据可查、边界靠契约；修复后有 SAFETY + 显式判空，评审可核。

**预防措施**：启用 `#![deny(clippy::undocumented_unsafe_blocks)]`（`[workspace.lints]`，见 `GOV-06`）——这是**用工具强制 SAFETY 注释**的最有效方式；对 `win-integration` 逐文件补齐（可作为一次性专项）。

---

# STD-10 · 事件主题 `operation.conflict` 零使用者（契约面漂移）　`P3` `[实测]`

**位置**：[host-core/src/events.rs:63](../../crates/host-core/src/events.rs#L63)（注册表声明 `("operation.conflict", "文件操作同名冲突，等待 UI 应答。payload: {op_id, target}", BackpressurePolicy::None)`）；全仓（含前端 TS）对该字符串引用为 0，唯一提及是 [file-core/src/conflict.rs:4](../../crates/file-core/src/conflict.rs#L4)"保留给未来的逐文件中断式询问"。

**问题**：注册表声明了一个"两端皆无"的主题（无发布者、无订阅者），与 `docs/impl/05-phase2-modules.md:104` 的规划脱节。

**解决方案**（二选一）：
1. **真接入**：按 `impl/05` 实现逐文件冲突询问（发布者：`file-core` 冲突点；订阅者：前端对话框 + `operation_conflict_respond` 命令），并补端到端测试。
2. **移除/标记**：从 `TOPIC_REGISTRY` 移除，并在 `impl/05` 标注"v1.1 规划（当前无发布者/订阅者）"，避免契约面误导。

**前后对比**：修复前注册表与实际能力不一致；修复后二者一致。

**预防措施**：把 `TOPIC_REGISTRY` 与代码的双向一致性做成**自动检查**（脚本：每个注册主题都要有"发布者文件"与"订阅者文件"命中，或显式列入"预留"清单）——本次可通过 grep 生成该报告，纳入 CI 输出即可（低成本、高收益）。

---

## 二、工程治理

# GOV-01 · 无 `rustfmt.toml`（但 CI 依赖 `cargo fmt --check`）　`P2` `[实测]`

**证据**：根目录无 `rustfmt.toml`/`.rustfmt.toml`；[.github/workflows/ci.yml:24](../../.github/workflows/ci.yml#L24) 执行 `cargo fmt --all --check`。

**影响**：格式规则取 rustfmt 默认值，随工具版本升级漂移；团队风格（行长、`newline_style`、import 分组）无法固定，跨平台（Windows 开发 / ubuntu CI）易产生噪声 diff。

**解决方案**：新增 `rustfmt.toml`：
```toml
edition = "2021"
max_width = 100
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
imports_granularity = "Crate"     # 若使用 nightly-only 选项需固定工具链或移除
```
（注意：`imports_granularity` 等属 nightly-only；若 CI 用 stable，请只保留 stable 支持的键，或显式 `rustup component add rustfmt --toolchain nightly`。）

**前后对比**：修复前"隐式默认"；修复后显式可复现。

**预防措施**：把"格式/风格配置"与 CI 步骤成对引入——凡 CI 里跑某工具，仓库就应有其配置文件（可作为"新增门禁步骤"的检查项）。

---

# GOV-02 · 无 `cargo-deny` / `npm audit`，与文档声明冲突　`P2` `[实测]`

**证据**：无 `deny.toml`；`ci.yml` 全流程无依赖审计；而 [THIRD_PARTY_LICENSES.md:6](../../THIRD_PARTY_LICENSES.md#L6) 称"`(未声明)` 条目由批次 2 的 cargo-deny 门禁复核"，[REVIEW-2026-09-18.md:240](../REVIEW-2026-09-18.md#L240) 也把 cargo-deny/npm audit 列为待建。

**影响**：259+ 依赖的许可合规（GPL-3.0 项目对外发布）与已知漏洞（RUSTSEC/GHSA）**无自动门禁**；`THIRD_PARTY_LICENSES.md` 的"复核"承诺无从执行。

**解决方案**
1. 新增 `deny.toml`：
```toml
[licenses]
allow = ["MIT","Apache-2.0","BSD-2-Clause","BSD-3-Clause","ISC","Zlib","Unicode-3.0","MPL-2.0","CC0-1.0"]
confidence-threshold = 0.9

[bans]
multiple-versions = "warn"
wildcards = "deny"

[advisories]
yanked = "deny"
ignore = []            # 需要忽略时逐条注明理由与到期时间

[sources]
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```
2. CI 增加步骤（放在 rust job 末尾或独立 job）：
```yaml
      - name: cargo-deny (licenses/advisories/bans)
        uses: EmbarkStudios/cargo-deny-action@v2
```
3. 前端：`npm audit --audit-level=high`（可在 frontend job 追加；若当前有历史告警，先设 `continue-on-error` 并建 issue 清单，再逐步清零）。
4. 反向同步 `THIRD_PARTY_LICENSES.md`：由 `cargo metadata` + `deny.toml` 结果驱动生成（`tools/gen-third-party-licenses.mjs` 已存在，可扩展为读 deny 配置）。

**前后对比**：修复前"声明有、实际无"；修复后许可与漏洞审计自动化，文档声明成立。

**预防措施**：文档中**禁止声明"未来会有"的机制**——要么落地并进 CI，要么在文档里标 `（v1.1 计划）`。这条规则可写进 `docs/` 的写作约定（本次审查中 `DOC-13`/`GOV-03` 属同类问题）。

---

# GOV-03 · 无 `.github/dependabot.yml`（与 DESIGN §7 声明冲突）　`P2` `[实测]`

**证据**：`.github/` 下仅 `workflows/`；[DESIGN.md:282](../DESIGN.md#L282) 称"依赖治理：dependabot 自动 PR + 每月集中处理"。

**解决方案**
```yaml
# .github/dependabot.yml
version: 2
updates:
  - package-ecosystem: cargo
    directory: "/"
    schedule: { interval: weekly, day: monday }
    open-pull-requests-limit: 10
    groups:
      rust-minor-patch: { patterns: ["*"], update-types: ["minor","patch"] }
  - package-ecosystem: npm
    directory: "/"
    schedule: { interval: weekly, day: monday }
    open-pull-requests-limit: 10
  - package-ecosystem: github-actions
    directory: "/"
    schedule: { interval: monthly }
```

**前后对比**：修复前依赖更新靠人工；修复后自动 PR + 分组减少噪声。

**预防措施**：CI 安全类工具（deny/audit/dependabot）一次性配齐（`GOV-02` + `GOV-03` 同批）；并在 `DESIGN.md §7` 的"依赖治理"段落给出指向实际配置文件的链接，避免再次脱节。

---

# GOV-04 · 缺协作与安全治理文件　`P2` `[实测]`

**证据**：无 `.github/CODEOWNERS`、`CONTRIBUTING.md`、`SECURITY.md`。

**影响**：GPL-3.0 对外仓库没有私密漏洞上报通道（研究者只能开 public issue，等于公开 0-day）；无评审归属（关键模块改动无人自动被通知）。

**解决方案**
1. `SECURITY.md`：披露邮箱（或 GitHub 私密漏洞报告开关）、支持版本、响应时限、`cargo-deny` 扫描结果说明。
2. `.github/CODEOWNERS`：
```
*                       @ZephyrWhispe
/crates/vault-core/     @ZephyrWhispe
/crates/kvm-core/       @ZephyrWhispe
/src-tauri/permissions/ @ZephyrWhispe
```
3. `CONTRIBUTING.md`：把本次审查沉淀的**编码约定**写进去（原子写、路径校验、回调不取锁、提权输入白名单、错误码与 hint、破坏性操作确认、`unsafe` 需 SAFETY 等）——这是 `07-prevention.md` 建议的落地载体。

**前后对比**：修复前无通道/无归属/约定散落；修复后有通道、有归属、约定成文。

**预防措施**：把"安全治理文件"纳入仓库初始化清单；每次审查产出的"预防措施"条目应**合并到 `CONTRIBUTING.md`**，避免只躺在报告里。

---

# GOV-05 · 无 `.gitattributes` / `.editorconfig`（跨平台噪声）　`P2` `[实测]`

**证据**：根目录二者均缺；仓库在 Windows 上开发，而 CI 在 ubuntu 上跑 `cargo fmt --check`/eslint。

**影响**：CRLF/LF 归一缺失 → 跨平台 fmt/lint 抖动与噪声 diff（本项目已有 `indexHtmlInlineStyle` 这类"打包期精密"的关注点，行尾不一致会额外制造麻烦）。

**解决方案**
```
# .gitattributes
* text=auto eol=lf
*.bat text eol=crlf
*.cmd text eol=crlf
*.ps1 text eol=crlf
*.png binary
*.ico binary
```
```ini
# .editorconfig
root = true
[*]
charset = utf-8
end_of_line = lf
insert_final_newline = true
indent_style = space
indent_size = 2
[*.rs]
indent_size = 4
[*.md]
trim_trailing_whitespace = false
```

**前后对比**：修复前行尾/缩进随平台而变；修复后一致。

**预防措施**：与 `GOV-01`（rustfmt.toml）同批落地；在 `CONTRIBUTING.md` 写明"提交前不需要手工调整行尾，由 `.gitattributes` 归一"。

---

# GOV-06 · 缺 MSRV、workspace lint 表与发布元数据　`P2` `[实测]`

**证据**：根 [Cargo.toml:5-8](../../Cargo.toml#L5-L8) 的 `[workspace.package]` 仅 `version/edition/authors`——无 `rust-version`、无 `license`/`repository`、无 `[workspace.lints]`；无 `clippy.toml`；`ci.yml:26` 仅用命令行 `-D warnings`。

**影响**：MSRV 无机器可判定（`DESIGN.md` 称 Rust 1.80+）；lint 规则不可跨 crate 统一（本次已发现 `unsafe` SAFETY 缺失、`unwrap` 非测试路径 9 处等问题，正需要 lint 约束）；发布元数据不含 GPL-3.0（与 `README.md:64` 的许可声明割裂）。

**解决方案**
```toml
# Cargo.toml
[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.80"
authors = ["ZephyrWhispe"]
license = "GPL-3.0-only"
repository = "https://github.com/ZephyrWhispe/NexusForge"

[workspace.lints.rust]
unsafe_code = "warn"
missing_debug_implementations = "warn"

[workspace.lints.clippy]
all = "warn"
undocumented_unsafe_blocks = "deny"     # 强制 SAFETY 注释（STD-09）
unwrap_used = "warn"                    # 生产代码收敛（COR-29）
expect_used = "warn"
```
各 crate 追加：
```toml
[lints]
workspace = true
```
（`unwrap_used`/`expect_used` 会命中大量测试代码与既有调用点，建议先 `warn` + 按 crate 逐步 `deny`，或用 `#[cfg(test)]` 豁免策略。）

**前后对比**：修复前 MSRV/lint/许可元数据缺失；修复后机器可判定、lint 统一、元数据与 README 一致。

**预防措施**：把"CI 用到的 lint 规则必须写进 `[workspace.lints]`"作为约定（命令行 `-D warnings` 只作为"告警即失败"的开关，规则集合应显式可版本化）。

---

# GOV-07 · 工作区遗留物与未跟踪的 `.zcodeignore`　`P2` `[实测]`

**证据**：根目录存在 `nf_t23_gates.log`（168KB）、`nf_t7_vitest.log`、`dist/`（13MB+ 构建产物）；三者被 `.gitignore` 的 `*.log`/`dist/` 忽略（`git status` 未见），但 **`.zcodeignore` 处于未跟踪状态**（`git status` 显示 `?? .zcodeignore`）且与 `.gitignore` 重复维护（其头部注释即声明"从 .gitignore 同步"）。

**影响**：日志/产物占用工作区且易被误入库（`git add -A` 场景）；`.zcodeignore` 双源漂移（同步机制无强制）。

**解决方案**
1. 清理：删除两个日志文件与 `dist/`（构建产物不进仓库；`dist/` 已被忽略，仍建议本地清理以免误导）。
2. `.zcodeignore` 二选一：纳入版本管理并明确"以 `.gitignore` 为源、本文件由其生成"；或移出仓库（若属个人工具配置）。
3. 在 `GOV-05` 的 `.gitattributes` 之外，把 `*.log`、`/dist/`、`/target/`、`/node_modules/` 一并核对 `.gitignore` 是否完整。

**前后对比**：修复前工作区残留 + 双源漂移；修复后干净且单一事实源。

**预防措施**：把"审查/诊断产生的日志文件"统一放到被忽略的目录（如 `.logs/`，且 `.logs/` 入 `.gitignore`），避免根目录堆积（本次审查已将原始日志放在 `docs/review-2026-09-25/_raw/`，建议后续统一为被忽略目录）。

---

# GOV-08 · CI 无基准执行与性能回归门禁　`P3` `[实测]`

**证据**：[ci.yml:30-31](../../.github/workflows/ci.yml#L30-L31) 仅 `cargo bench --workspace --no-run`（**只编译不执行**）；[DESIGN.md:354](../DESIGN.md#L354) 称"性能回归 | CI 每次提交跑基准：启动/内存/搜索响应/模块加载，退化超阈值即失败"。阈值断言实际由 `crates/clipboard-core/tests/perf_thresholds.rs` 随 `cargo test` 承担（p95 阈值 50/100ms 与 §9.1 一致 `[实测]` 存在）。

**影响**：文档承诺的"退化超阈值即失败"在 CI 中不存在（仅编译校验 + 部分阈值断言）；启动与内存无自动化（D-27 决策 3 已记 v1.1）。

**解决方案**（按成本递进）
1. 修正文档口径（最小动作）：§9.2 改为"基准矩阵仅编译校验（`ci.yml`）；阈值断言随 `cargo test` 常跑（`perf_thresholds.rs`）；启动/内存记 v1.1（D-27）"。
2. 增加可执行的基准回归：独立 nightly job 跑 `cargo bench` 并与缓存 baseline 比较（criterion 支持 `--save-baseline`/`--baseline`），退化超阈值时失败。
3. 启动耗时：加一个"实启冒烟"用例（项目已有 PERF1 实启冒烟的记录），断言冷启动在阈值内（用宽松阈值避免 runner 抖动）。

**前后对比**：修复前承诺与实现不符；修复后文档与 CI 一致，且（可选）有真正的回归门禁。

**预防措施**：**文档中的"验收标准"必须指向可执行的 CI 步骤或测试文件**；写完设计文档后应反向核对（本次 `DOC-13` 即此类偏差，建议在文档评审时加"可验证性"检查）。

---

# GOV-09 · `ipc_contract.rs` 仅覆盖 4 个命令（255 条命令零契约测试）　`P3` `[实测]`

**证据**：[src-tauri/tests/ipc_contract.rs:14-16](../../src-tauri/tests/ipc_contract.rs#L14-L16) 仅 `use …{build_modules_status, host_system_accent, vault_generate_password, vault_totp_now}` 并断言这 4 个纯函数语义。而 `lib.rs` 的 `generate_handler!` 注册 255 条命令、`permissions/*.toml` 也恰好 255 条（`security_config.rs:261` 覆盖的是 ACL 权限**名**，非命令签名）。

**影响**："S6 IPC 契约回归"名不副实——签名漂移（前端 `invoke` 参数名/形状与后端不匹配）无回归保护。这类漂移在运行时才暴露，且往往发生在错误路径（用户点了才发现）。

**解决方案**
1. **从单一事实源生成契约清单**（推荐）：写脚本从 `commands/*.rs` 提取 `#[tauri::command]` 名 + 参数名 + 返回类型，生成 `ipc_contract.json` 并与前端 `src/ipc/client.ts` 的 DTO/调用参数做比对；CI 断言二者一致。
2. **最小可落地版**：断言"命令名集合 == permissions 中的 `allow-*` 集合"，并断言"前端 `client.ts` 中出现的每个 `invoke("…")` 名都在命令集合内"（用正则 + 集合差集，成本低但能挡住重命名漂移）：
```rust
#[test]
fn every_frontend_invoke_target_exists_in_registry() {
    let registry = command_names_from_lib_rs();                    // 解析 generate_handler! 或 allowlist 文件
    let src = std::fs::read_to_string("../../src/ipc/client.ts").unwrap();
    let invoked: std::collections::HashSet<String> =
        regex::Regex::new(r#"invoke<[^>]*>\(\s*"([a-z0-9_]+)""#).unwrap()
            .captures_iter(&src).map(|c| c[1].to_string()).collect();
    let missing: Vec<_> = invoked.difference(&registry).collect();
    assert!(missing.is_empty(), "前端调用了不存在的命令: {missing:?}");
}
```
3. 对高风险命令（`vault_*`、`file_*` 的路径入参、`winops_*`）补"参数校验负数例"测试（与 `GOV-02` 的安全方向一致）。

**前后对比**：修复前 4/255 覆盖；修复后至少覆盖"命名与签名一致性"（低成本的 80% 价值），高风险命令另有负例。

**预防措施**：把"IPC 契约测试"作为新增命令的**必配项**（可在 `CONTRIBUTING.md` 约定：新增 `#[tauri::command]` 必须同步更新契约夹具）；长期建议把命令注册表改为**数据驱动**（命令表 + 自动生成权限文件与契约测试），从结构上消除三方漂移（命令实现 / permissions / 前端客户端）。