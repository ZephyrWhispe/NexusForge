# 04 · 性能问题详解

> 覆盖 `PERF-01`…`PERF-10`。每条含：问题描述 → 解决方案 → 修复前后对比 → 预防措施。
> 说明：本次审查**未执行基准测试**（`cargo bench` 仅编译校验，见 `GOV-08`）。因此本文的严重度基于**路径热度**（是否在系统输入路径 / UI 主线程 / 高频事件循环内）判定，而非实测耗时；落地修复后应补基准或 profiling 复测。

---

# PERF-01 · 输入钩子回调内加锁（阻塞全局键鼠）　`P1` `[实测]`

### 位置
[crates/kvm-core/src/module.rs:839-862](../../crates/kvm-core/src/module.rs#L839-L862)

### 问题描述

```rust
// module.rs:839-862
hook.start_capture(Box::new(move |ev: &RawInput| {
    let decision = {
        let mut es = es_for_cb.lock();        // ← 低级钩子线程内取锁
        es.on_local_event(ev, &rect)
    };
    match decision {
        Decision::Forward => {
            let device = es_for_cb.lock().controlling_device()   // ← 同回调内第二次取锁
                .unwrap_or_default().to_string();
            let _ = tx_for_cb.send(Cmd::Forward(ev.clone(), device));
            false
        }
        ...
    }
}))
```

`WH_KEYBOARD_LL`/`WH_MOUSE_LL` 回调在**系统输入路径上同步执行**——回调不返回，后续输入事件就会被阻塞（超过 LowLevelHooksTimeout 后系统还会静默丢弃钩子）。而 worker 侧（[:895](../../crates/kvm-core/src/module.rs#L895)、[:923](../../crates/kvm-core/src/module.rs#L923)）也持同一把 `edge_switch` 锁。

值得注意的是：`win-integration` 自身已按 D-15 改为 `ArcSwap` 无锁快照（[input.rs:73-75](../../crates/win-integration/src/input.rs#L73-L75)），**其调用方（KVM 模块）却仍在回调内加锁**——这正是"锁策略改造只做了一半"的证据（前次报告称 D-15 已落地，见 [README §4.3](./README.md)）。

### 影响
- 锁竞争时（worker 正在处理转发/切换/边缘判定）回调被阻塞 → **全系统键鼠卡顿/丢输入**，体感最差的一类性能问题。
- 与"接管模式"（`Decision::Forward`，每次事件都要取锁 + 克隆 + 发消息）叠加，问题在高频鼠标移动时最明显。

### 解决方案

**步骤 1：把回调内的判定状态改为无锁快照（`ArcSwap`，与 `input.rs` 一致）**

```rust
// 决策所需的只读状态抽成不可变快照
#[derive(Clone)]
struct EdgeSnapshot {
    controlling: Option<String>,
    local_rect: ScreenRect,
    threshold: i32,
    capture_enabled: bool,
}

// EdgeSwitch 内部：snapshot: ArcSwap<EdgeSnapshot>（worker 侧更新 → 原子发布）
hook.start_capture(Box::new(move |ev: &RawInput| {
    let snap = es_snapshot.load();                 // 无锁读：仅原子指针交换
    match decide_local(ev, snap.as_ref()) {        // 纯函数，不做 IO、不取锁
        Decision::Passthrough => true,
        Decision::Suppress => false,
        Decision::Forward => {
            let device = snap.controlling.clone().unwrap_or_default();
            let _ = tx_for_cb.send(Cmd::Forward(ev.clone(), device));   // 无界 channel 发送（非阻塞）
            false
        }
        Decision::SwitchTo(d) => { let _ = tx_for_cb.send(Cmd::Switch(d)); true }
    }
}))
```

**步骤 2：若判定必须依赖可变状态（累积位移、边缘计时）**，则改为"回调只做**采集 + 无锁投递**，决策全在 worker"：

```rust
// 回调：仅拷贝事件到 SPSC 队列（crossbeam ArrayQueue / tokio unbounded）
static_QUEUE.push(RawInputOwned::from(ev));
true                                  // 立即放行，决策与抑制由 worker 下发"下一步是否抑制"
```
> 该方案能彻底把钩子线程做到"只 push"，但引入"抑制决策滞后一帧"的语义，需与产品确认（KVM 接管通常可接受）。

**步骤 3：为回调路径加"禁止分配/禁止锁/禁止 IO"的守护测试**

```rust
#[test]
fn hook_callback_path_is_lock_free_and_allocation_light() {
    // 断言 decide_local 为纯函数；用 Fake 快照压测 10 万次，统计耗时与分配（可用 dhat / stats_alloc）
}
```
（可选：在 CI 里用 `cargo bench` 对 `decide_local` 做 p99 断言，复用项目已建的 `perf_thresholds` 阈值断言模式。）

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| 回调内锁 | 2 次 `Mutex::lock()` | 0 次（`ArcSwap::load` 无锁） |
| 回调内分配 | `device.to_string()`、`ev.clone()` | 仅必要的 `clone`（或按方案 2 降为队列 push） |
| 全局键鼠延迟 | 受 worker 锁持有时间影响 | 与 worker 解耦 |
| 与 `win-integration` 一致性 | 不一致（input 无锁、调用方加锁） | 一致 |

### 预防措施
1. **"回调/钩子/中断路径"单列一份代码规则**：禁止 `Mutex::lock`、`RwLock::write`、文件/网络 IO、`format!` 大字符串、`Vec` 扩容；在 review 清单中作为必检项（项目已有无数次"回调内不取锁"的教训记录）。
2. **`ArcSwap` 快照模式沉淀为模板**：`input.rs` 已是正确范例，应把"状态 = 不可变快照 + 原子发布"写成 `host-core` 的通用模式文档。
3. **加关联测试**：任何注册到系统钩子的回调，都要有"压测 + 无锁断言"。

---

# PERF-02 · 设置表单每键 IPC + 落盘（且失败不回滚）　`P1` `[实测]`

### 位置
[src/settings/SchemaForm.tsx:118-124](../../src/settings/SchemaForm.tsx#L118-L124)、[:178-191](../../src/settings/SchemaForm.tsx#L178-L191)

### 问题描述

```tsx
// :118-124
const save = (next: Values) => {
  setValues(next);
  hostConfigSet(moduleId, next).then(...).catch((e) => setError(...));   // 每次调用即一次 IPC
};
// :178-191
<Input onChange={(_, d) => save({ ...values, [key]: d.value })} />       // ← 每击键一次（含 SpinButton/Textarea）
```
无防抖、无在途合并、无"仅提交变更字段"；失败时只 `setError`，**乐观更新的值不回滚**（界面继续显示"已保存"的值）。

### 影响
- 输入一个 10 字符的值 = 10 次 IPC + 10 次配置落盘（`hostConfigSet` 会写配置文件；配置派发还会触发模块 `apply_config`，见 `PERF-08`）。
- 失败场景（权限/磁盘）下用户以为已保存，实际未保存——**正确性问题与性能问题叠加**。
- 该 IPC 会触发 `state.rs` 的配置分发（`spawn_config_feed`），进一步放大成本。前次报告 U7 已点出，**未修**。

### 解决方案

**步骤 1：本地草稿 + 防抖提交 + 失败回滚**

```tsx
const [values, setValues] = useState(initial);
const [draft, setDraft] = useState(initial);
const [savingKeys, setSavingKeys] = useState<Set<string>>(new Set());
const pendingRef = useRef<Record<string, unknown>>({});
const timerRef = useRef<number>();

const flush = useCallback(async () => {
  const patch = pendingRef.current;
  pendingRef.current = {};
  if (Object.keys(patch).length === 0) return;
  const prev = values;
  const next = { ...values, ...patch };
  setValues(next);                                   // 乐观更新
  setSavingKeys(new Set(Object.keys(patch)));
  try {
    await hostConfigSet(moduleId, patch);            // 只提交变更字段
  } catch (e) {
    setValues(prev);                                 // ← 失败回滚
    reportError(e, { context: "设置保存失败", hint: "该修改未生效，请重试" });
  } finally {
    setSavingKeys(new Set());
  }
}, [moduleId, values]);

const onChange = (key: string, v: unknown) => {
  setDraft((d) => ({ ...d, [key]: v }));
  pendingRef.current[key] = v;
  window.clearTimeout(timerRef.current);
  timerRef.current = window.setTimeout(() => void flush(), 400);   // 400ms 防抖
};
// 卸载/切换模块前强制 flush（避免丢失最后一次输入）
useEffect(() => () => { window.clearTimeout(timerRef.current); void flush(); }, [flush]);
```
（提交接口若支持"变更字段 patch"，应确认后端 `apply_config` 语义为**合并**而非整体替换；若为替换，需先取完整配置再合并。）

**步骤 2（可选）：为布尔/下拉等"离散且低价"的控件保留即时提交**（点击即意图明确、频率低），只对文本/数字/多行做防抖——按控件类型分流。

**步骤 3：可见性反馈** —— 保存中显示行内 spinner，成功后短暂"已保存"，失败行内标红（`savingKeys` 已提供状态）。

**步骤 4：测试**

```tsx
it("连续输入只触发一次配置写入", async () => {
  const spy = vi.spyOn(ipc, "hostConfigSet").mockResolvedValue(undefined);
  await typeIn(container, "abc");          // 触发 3 次 onChange
  await vi.advanceTimersByTimeAsync(500);
  expect(spy).toHaveBeenCalledTimes(1);
});
it("保存失败时回滚输入值", async () => { /* 断言 input.value 回到旧值 + 出现错误提示 */ });
```

### 修复前后对比

| 维度 | 修复前 | 修复后 |
|---|---|---|
| IPC 次数 | 每击键 1 次 | 每 400ms 空闲 1 次（≈1/字符数） |
| 落盘次数 | 每击键 1 次 | 同 IPC |
| 失败处理 | 仅提示，值表面仍"已保存" | 回滚 + 行内报错（hint 可操作） |
| 离线/卸载 | 最后一次输入可能丢失 | 卸载前强制 flush |
| 测试 | 无（仅渲染断言） | 新增 2 条行为断言 |

### 预防措施
1. **"输入即写盘"统一禁止**：把"表单 → 持久化"的路径统一为"草稿状态 + 显式保存或防抖提交"，在 `src/components` 提供 `useDebouncedSave` 复用。
2. **配置写入合并**：后端 `apply_config` 应支持"部分字段更新"，避免"每次提交整份配置"带来的额外校验与派发开销。
3. **失败必回滚**：把"乐观更新必须实现回滚"作为 review 项（本次审查还发现 `PERF-06`/`COR-16` 等乐观路径，可统一整改）。

---

# PERF-03 · Monaco 全量入口 + 无 `manualChunks`（前次 M10 未修）　`P2` `[实测]`

**位置**：[src/monaco/setup.ts:6](../../src/monaco/setup.ts#L6)、`vite.config.ts:16-21`

**问题**：`import * as monaco from "monaco-editor"` 打进全量编辑器（含全部 basic-languages 与所有 worker）；`vite.config.ts` 的 `build` 段只有 `target/outDir/emptyOutDir`，**无 `rollupOptions.manualChunks`**。前次报告实测 `EditorPanel` chunk ≈3.24MB、`ts.worker` ≈5.87MB。

**影响**：编辑器面板首次进入需下载/解析数 MB；打包产物体积大（`dist` 曾 13MB+），影响安装包与冷启动。

**解决方案**：
1. 按需引入编辑器内核并只注册需要的语言/特性：
```ts
// src/monaco/setup.ts
import * as monaco from "monaco-editor/esm/vs/editor/editor.api";
import "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution";
import "monaco-editor/esm/vs/basic-languages/javascript/javascript.contribution";
import "monaco-editor/esm/vs/basic-languages/json/json.contribution";
// 只启用需要的特性（查找/折叠等），跳过不必要的 language services
```
2. 拆包（把大依赖从主 chunk 分离，利用已有的路由级 `lazy()` 做加载边界）：
```ts
// vite.config.ts
build: {
  target: "chrome105", outDir: "dist", emptyOutDir: true,
  rollupOptions: {
    output: {
      manualChunks(id) {
        if (id.includes("monaco-editor")) return "monaco";
        if (id.includes("@xterm")) return "xterm";
        if (id.includes("@fluentui")) return "fluentui";
        if (id.includes("node_modules")) return "vendor";
      },
    },
  },
}
```
3. 为 worker 明确路径（若走了 `esm` 入口，Monaco 的 worker 需用 Vite 的 `?worker` 方式声明），并确认 `worker-src blob:` 在 CSP 中已允许（现 CSP 已含）。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 编辑器 chunk | ≈3.24MB 全量 | 按需语言 + 独立 `monaco` chunk（按路由懒加载） |
| 首屏 | 不直接受影响（已 lazy），但缓存粒度差 | 依赖分包，缓存命中更好 |
| 构建可见性 | 无分包配置 | `manualChunks` + 体积回归可观测 |

**预防措施**：在 CI 增加"产物体积预算"检查（`vite build` 后断言 `dist` 关键 chunk 不超阈值，或引入 `rollup-plugin-visualizer` 产物对比），把"体积回归"变成红灯；`GOV-08` 的性能门禁建议一并覆盖前端。

---

# PERF-04 · 文件目录列表未虚拟化　`P2` `[实测]`

**位置**：[file/FilePanel.tsx:953-1001](../../src/modules/file/FilePanel.tsx#L953-L1001)、[file/RemoteBrowser.tsx:299-331](../../src/modules/file/RemoteBrowser.tsx#L299-L331)

**问题**：`entries.map(...)` 直接渲染 `<TableRow>`，条目数无上限（本地目录可达数万）。剪贴板历史已正确使用 `@tanstack/react-virtual`（`HistorySection`），文件面板未用。

**影响**：大目录下渲染与滚动卡顿；选中态变更（`selected.has(e.path)`）会触发全量行重渲染。

**解决方案**：
```tsx
const rowVirtualizer = useVirtualizer({
  count: entries.length,
  getScrollElement: () => tableWrapRef.current,
  estimateSize: () => 32,          // 固定行高（表格行可固定）
  overscan: 12,
});
// 用绝对定位渲染可见行，或按窗口切片 entries.slice(start, end)
{rowVirtualizer.getVirtualItems().map((vi) => {
  const e = entries[vi.index];
  return <TableRow key={e.path} style={{ height: vi.size, transform: `translateY(${vi.start}px)` }} ... />;
})}
```
并把 `selected` 从 `Set` 的比较改为"行组件只订阅自身是否被选中"（`React.memo` + 传 `isSelected` 布尔），避免全量重渲染。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| DOM 行数 | 全部条目 | 可见区 + overscan（≈30 行） |
| 万级目录 | 明显卡顿 | 恒定渲染成本 |
| 选中变更 | 全量行重渲染 | 仅受影响行 |

**预防措施**：把"列表渲染"约定为"超过 N（如 200）条必须虚拟化"；在 `src/components` 提供 `<VirtualList>` 包裹组件，避免各面板各写一套；review 时对 `entries.map(` 模式提问"条目上限是多少"。

---

# PERF-05 · OCR 先整图解码、后缩放（内存放大）　`P2` `[走查]`

**位置**：[ocr-core/src/module.rs:188](../../crates/ocr-core/src/module.rs#L188)、[:522-527](../../crates/ocr-core/src/module.rs#L522-L527)

**问题**：
```rust
let (w, h, rgba) = decode_rgba(bytes)?;                     // ← 整图解码进内存
let (fw, fh, frame_rgba) = downscale_if_needed(w, h, rgba); // ← 之后才按 4096 上限缩放
```
`decode_rgba` 内部 `image::load_from_memory` + `to_rgba8`。一张 20000×20000 的 PNG 会先分配 ≈1.6GB RGBA，再缩图。`OcrRequest.image_b64` 来自 IPC，**无字节/像素预算**。

**影响**：恶意或异常大图直接 OOM/长时间卡顿；OCR 面板一次误操作即可让应用内存飙升。

**解决方案**：
```rust
/// 解码前先用 header 读尺寸做预检，并限制输入字节
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PIXELS: u64 = 40_000_000;          // ≈ 6320×6320

fn decode_with_budget(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    if bytes.len() > MAX_IMAGE_BYTES { return Err(OcrError::Input("图像超过 64MB 上限")); }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;                       // 只读 header，不解码像素
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(OcrError::Input(format!("图像分辨率 {w}×{h} 超过上限（{MAX_PIXELS} 像素）")));
    }
    // 解码时直接缩采样（image crate 的 thumbnail/超采样），避免"先全尺寸再缩"
    let img = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?
        .decode()?
        .resize(OCR_MAX_SIDE, OCR_MAX_SIDE, image::imageops::FilterType::Triangle);
    let rgba = img.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}
```
命令层同时对 `image_b64` 的 base64 长度设上限（解码前拒绝）。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 峰值内存 | 与原始分辨率成正比（可达 GB） | 与目标尺寸成正比（几十 MB） |
| 超大图行为 | OOM/卡顿 | 明确拒绝（code + hint） |
| IPC 入参 | 无限制 | 字节上限 + 像素上限双闸 |

**预防措施**：为所有"接收图像/压缩包/文本"的 IPC 定义**输入预算表**（字节、像素、条目、字符数），并在 `DESIGN.md §8` 记录；`screenshot` 捕获路径（本地受信）可放宽，但来自 IPC/远端的图像必须过预算。

---

# PERF-06 · TOTP 每徽章 1Hz IPC 轮询　`P2` `[实测]`

**位置**：[vault/VaultPanel.tsx:161-190](../../src/modules/vault/VaultPanel.tsx#L161-L190)（`setInterval(tick, 1000)`）

**问题**：每个含 TOTP 的条目各自 `setInterval(tick, 1000)`，每秒 `vaultTotpNow(secret)` 一次 invoke 并 `setState`。而 TOTP 默认 30s 周期，只有跨周期才需重算。

**影响**：N 个 TOTP 条目 = N 次/秒 IPC + 每秒 N 次组件重渲染（VaultPanel 是 1072 行的大组件，重渲染成本不低）。

**解决方案**：
```tsx
// 按"剩余秒数"倒计时，而不是 1Hz 轮询后端；仅跨周期时重新取码
useEffect(() => {
  let timer: number;
  const schedule = (remaining: number) => {
    timer = window.setTimeout(async () => {
      const [code, rem] = await vaultTotpNow(secret);
      setCode(code);
      schedule(rem);                       // 到点再取下一次，而非每秒
    }, Math.max(250, remaining * 1000 + 150));   // 边界留 150ms 余量
  };
  void vaultTotpNow(secret).then(([code, rem]) => { setCode(code); schedule(rem); });
  return () => window.clearTimeout(timer);
}, [secret]);
```
若需显示"剩余沙漏"的秒级跳动，用**本地** `Date.now()` 推算（不请求后端），只把"取码"改为跨周期触发。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| IPC/秒 | N（每条目） | ≈0（每 30s 每条目 1 次） |
| 渲染/秒 | N 次 | 仅到期时 1 次（倒计时可用 CSS/局部 state 承载） |
| 电量/CPU | 持续 | 接近静默 |

**预防措施**：禁止用固定短间隔轮询"变化很慢的数据"；`DESIGN.md §7` 已要求事件驱动，应补充"轮询只用于不可事件化的指标，且间隔需与数据变化频率匹配"的评审项；`PERF-10` 指出总线已有节流能力却无人使用，可优先消费在该类场景。

---

# PERF-07 · 自动化动作重试用 `thread::sleep` 阻塞 tokio worker　`P2` `[实测]`

**位置**：[automation-core/src/engine.rs:120-124](../../crates/automation-core/src/engine.rs#L120-L124)（`std::thread::sleep`），调用点 [module.rs:401](../../crates/automation-core/src/module.rs#L401)（`engine.fire(...)` 同步调用）

**问题**：
```rust
Err(e) if attempt < ACTION_RETRIES => {
    attempt += 1;
    std::thread::sleep(Duration::from_millis(500u64 << (attempt - 1)));   // ← 阻塞
}
```
`fire` 在 tokio 任务里被同步调用 → 单个失败动作最多阻塞 ~1.5s（0.5+1s）。

**影响**：多规则/多失败并发时挤占 tokio worker（默认 = CPU 核数），拖慢同运行时上的其他任务（含事件转发、订阅处理）；极端情况下造成"整个自动化模块变慢"的观感。

**解决方案**：
1. 动作执行整体移到阻塞池：
```rust
// module.rs:401 附近
let engine = engine.clone(); let rule = rule.clone(); let ev = ev.clone();
tokio::task::spawn_blocking(move || engine.fire(&rule, &ev)).await??;
```
2. 或把重试改为异步（`fire` 异步化，用 `tokio::time::sleep`）。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 阻塞 | tokio worker 被 sleep 占住 | sleep 发生在 blocking 池线程 |
| 并发影响 | 多规则互相拖慢 | 与其他任务解耦 |

**预防措施**：确立"async 上下文中禁止 `std::thread::sleep`/`std::fs`/`rusqlite` 同步调用"的规则（与 `PERF-08` 同一议题），并加 CI 源码扫描断言（`grep -rn "std::thread::sleep" crates/*/src` 需逐条豁免）。

---

# PERF-08 · 配置派发与 sync 订阅在 async 上下文做阻塞 IO　`P2` `[实测]`

**位置**：[host-core/src/registry.rs:164](../../crates/host-core/src/registry.rs#L164)、[:181](../../crates/host-core/src/registry.rs#L181)；[sync-core/src/module.rs:1382](../../crates/sync-core/src/module.rs#L1382)、[:1462](../../crates/sync-core/src/module.rs#L1462)

**问题**：`init/start/stop` 都经 `spawn_blocking` 调用，唯独 `apply_config` 例外，直接在工作线程上执行（sync 的 `apply_config` 内真做 SQLite `DELETE`；sync 订阅任务里 `record_change_event` 做文件读 + `OpLog::append` 写库）。`ports.rs:6` 自定规约写明"阻塞调用由调用方放入 spawn_blocking"。

**影响**：一次配置写入可阻塞运行时工作线程（含事件转发）数毫秒~数十毫秒；配合 `PERF-02`（每击键都触发配置派发）叠加放大。

**解决方案**：
```rust
// host-core/src/registry.rs
let r = tokio::task::spawn_blocking(move || module.apply_config(values)).await;   // apply_configs
...
let r = tokio::task::spawn_blocking(move || module.apply_config(values)).await;   // apply_one

// sync-core 订阅任务内
let ctx2 = ctx2.clone();
tokio::task::spawn_blocking(move || record_change_event(&ctx2, &event)).await;
```
（注意 `apply_config` 需要 `module: Arc<dyn Module>` 的 `'static` 传递；现有 `spawn_config_feed` 已是 async 任务，改造量小。）

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 阻塞位置 | 运行时工作线程 | 阻塞线程池 |
| 对事件转发影响 | 可能延迟 | 无 |
| 规约一致性 | 违反 `ports.rs:6` | 一致 |

**预防措施**：`#[tauri::command]` 与模块 trait 两者的"阻塞 IO 清单"都要显式化；建议在 `ports.rs` 的 trait 方法上加文档标记（如 `/// [blocking]`），并在 review 清单要求"标注为 blocking 的方法必须以 `spawn_blocking` 调用"。

---

# PERF-09 · DIB 主线程同步解码 + 缩略图无缓存　`P2` `[实测]`

**位置**：[src/modules/clipboard/dib.ts:33-63](../../src/modules/clipboard/dib.ts#L33-L63)（逐像素循环）、[src/modules/clipboard/DibThumb.tsx:23-37](../../src/modules/clipboard/DibThumb.tsx#L23-L37)（按行请求、无缓存）

**问题**：
```ts
for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) { ... img.data[di] = bytes[o + 2]; ... }   // 同步 O(w*h)
```
8192×8192 的 DIB 约 6700 万次循环，**在主线程**执行；`DibThumb` 每次挂载都重新请求图像（无缓存），列表滚动会反复请求。

**影响**：大图粘贴后 UI 明显卡顿（甚至"未响应"）；滚动剪贴板历史时 IPC 与解码反复发生。

**解决方案**：
1. 解码移入 Web Worker 或使用浏览器原生能力：
```ts
// 优先：把 DIB 转成 ImageBitmap（在 worker 内或经 OffscreenCanvas），避免 JS 逐像素循环
const bitmap = await createImageBitmap(new ImageData(rgba, w, h));
// 或 Worker 内解码后 transfer ArrayBuffer 回主线程
```
2. 缩略图缓存（LRU，按条目 id + 尺寸）：
```ts
const thumbCache = new Map<string, ImageBitmap>();     // 或使用 react-query 的 cache
// 超过上限时淘汰最久未用项；条目删除时主动清理
```
3. 列表侧仅渲染可见项的缩略图（虚拟化后天然满足），并设置 `loading` 占位避免布局跳动。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 解码线程 | 主线程 | Worker / 原生 API |
| 大图 | 主线程卡顿（数秒） | 不阻塞 UI |
| 重复请求 | 每次挂载都请求 | LRU 命中，滚动零请求 |
| 内存 | 泄漏风险（无淘汰） | 有界（LRU 上限） |

**预防措施**：把"像素级处理"一律移出主线程（Web Worker/`createImageBitmap`/WASM）；为所有"列表内媒体"建立缓存 + 淘汰策略；在 `07-prevention.md §4` 的清单中登记"逐像素循环"为 review 关注点。

---

# PERF-10 · 总线节流/去抖能力（`Throttled`）无任何生产调用方　`P3` `[实测]`

**位置**：[host-core/src/events.rs:321](../../crates/host-core/src/events.rs#L321)（`subscribe_throttled`）、[:377](../../crates/host-core/src/events.rs#L377)（`subscribe_debounced`）、[:39](../../crates/host-core/src/events.rs#L39)（`BackpressurePolicy::Throttled`）

**问题**：三者仅出现在 `events.rs` 自身的测试中（`:560`、`:594`、`:745`），`TOPIC_REGISTRY` 中**无任何主题使用 `Throttled`**（全部为 `None`/`Merged`/`Batched`）。前次报告 D-03 声称"总线统一背压 API 并接线四个生产端"——接线的是生产侧合并（`Merged`）与批窗口（`Batched`），**消费侧节流/去抖是死面**。

**影响**：一是"能力已有却没人用"，高频主题（如 `sys.metrics`、`screenshot.*`）在 UI 侧只能逐条接收转发；二是契约面与事实面漂移（文档/决策声称的能力实际未被消费），后续开发者会误以为"高频订阅已受保护"。

**解决方案**（二选一，取决于是否真需要）：
1. **真接线**：为高频主题补消费端节流，例如托盘/前端转发订阅 `sys.metrics` 时使用 `subscribe_throttled(interval_ms)`；由 `TOPIC_REGISTRY` 声明策略并让统一订阅入口自动应用（避免"调用方忘了节流"）：
```rust
// host-core/src/events.rs —— 让 registry 策略与消费入口绑定
pub fn subscribe_with_policy(&self, topic: &str) -> Result<Subscription> {
    match policy_of(topic) {
        BackpressurePolicy::Throttled { interval_ms } => self.subscribe_throttled(topic, interval_ms),
        BackpressurePolicy::Batched { .. } => self.subscribe_batched(topic),
        _ => self.subscribe(topic),
    }
}
```
2. **删除或标注预留**：若短期内确实不需要，删除死代码或明确标注"预留（未接线）"，并在 `DECISIONS.md` 记录，避免"已实现即已生效"的误解。

**前后对比**：
| 维度 | 修复前 | 修复后 |
|---|---|---|
| 消费端节流 | 无调用方（死面） | 由主题策略自动应用（或显式标明预留） |
| 契约一致性 | 文档称"统一背压入口" | 与实现一致 |
| 高频主题 | 逐条转发 | 按策略节流/合并 |

**预防措施**：**"能力实现即必须有消费者或明确标记为预留"**——建议在 `DECISIONS.md` 里为每个"基础设施型能力"登记"当前消费者清单"，无消费者则记 `v1.1`；本次审查发现的同类"死面"还包括 `AlertError` 的 `retryable`（前端仅 1 处渲染）、`operation.conflict` 主题（`STD-10`）、`ModulePlaceholder`（`STD-08`），可一并清理。