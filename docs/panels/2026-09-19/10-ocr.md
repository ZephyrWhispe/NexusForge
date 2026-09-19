# 10 OCR 面板详细设计（B0 补壳 + B4 深化）

> 总纲：docs/impl/09 §9（与截图同批）；对标：蓝本 §3.10（Umi-OCR+PaddleOCR）+ eSearch/Textract；状态：未开工。与 09 同病根：后端管线完整（engine.rs/pipeline.rs/module.rs），`src/modules/` **无 ocr 目录**，主窗落"模块界面待实现"兜底。PaddleOCR 与翻译维持 D-08 v1.1 裁决不动（挂 DeferredBadge，动工前需独立 sidecar 子方案文档）。

## 1. 现状问题

- **零 UI 壳**：`ocr_recognize`/`ocr_engine_status`/`ocr_copy_text` 三命令（lib.rs:155-157）仅 OverlayShot 内部走 ocr_requested 事件链，用户无法主动"选一张图来识别"，引擎状态与首选引擎（`set_preferred` engine.rs:76）无处查看/设置。
- **langs 恒空**：`EngineRegistry::pick(langs)` 有按语言择优能力（engine.rs:95），但覆盖层提交 `langs: []`（OverlayShot.tsx:875）——多语言择优整条分支今天不生效。
- **结果即弃**：`OcrResultDto` 携带逐行 rect+confidence（types.rs:19-35，可块选复制的原料齐全），面板侧无任何渲染；仅合并文本经 `record_ocr_text` 回填截图历史（commands/ocr.rs:20），OCR 自身无任务历史。
- **能力面窄**：单图请求单图响应，无批量（文件夹任务队列）、无 PDF 输入（editor lopdf 可抽内嵌文本，与 OCR 互补的"失败才识别"路线未做）、无二维码/条码（rqdecode/zxing 类 crate 即可，WinOCR 不覆盖）、无表格/排版模式开关（`merge_text`/`segment_lines` pipeline.rs:68/119 是固定策略，不可配）。
- **预处理名不副实**：pipeline 头注释宣称"预处理"（pipeline.rs:1），实际无灰度/二值化/去噪/倾斜校正（蓝本 §3.10 识别管线明列）。

## 2. 子面板信息架构

| 子面板 | 类型 | 内容 |
|--------|------|------|
| 识别 | 编辑 | 三种进料：拖图/粘贴剪贴板图/框选屏幕（跳覆盖层 ocr 模式回传）；左侧原图右侧结果**双栏对照**——点击文本行高亮对应 rect、框选块复制、置信度色标（低置信词下划线）；输出模式：段落合并/逐行/Markdown 表格（rect 行列聚类）；二维码命中时优先直读 |
| 批量 | 清单 | 文件夹任务队列：入队/并发数/进度/失败重试；导出全目录 txt/md/csv（含坐标）；完成通知+归档目录 |
| 引擎 | 概览+设置 | 引擎表（id/available/注册顺序=优先级，`ocr_engine_status` 接线）+ 首选引擎下拉（set_preferred）+ **语言包管理**：WinOCR 系统语言列表（languages 字段）、Tesseract tessdata 目录选择与下载指引（v1 手动指路，自动下载登记 v1.1）|
| 历史 | 清单 | OCR 任务时间线（图缩略/引擎/语言/字数/耗时/来源=面板 or 截图链）+ 复制/重识别/删除；与截图历史互跳（同图同 id 关联，ShotStore 旁表）|
| 设置 | 设置 | 默认输出模式、语言偏好序（langs 来源，修 OverlayShot 恒空→从配置注入）、预处理开关（灰度/放大 2x/去噪）、翻译位 DeferredBadge（D-08 指回 DECISIONS）|

## 3. 对标功能矩阵

| # | 功能 | 来源 | 现状 | 实施 |
|---|------|------|------|------|
| 1 | OcrEngine trait + 可插拔注册表 | 蓝本/Kreuzberg | ✅ engine.rs | — |
| 2 | WinOCR 引擎 | 蓝本"轻量 fallback"位 | ✅ 唯一 | 保留 |
| 3 | Tesseract 引擎 | 蓝本 | ❌ | B4：FFI 或 `tesseract-rs`，作为注册表第二项，语言择优立即有意义 |
| 4 | PaddleOCR 主力中文 | 蓝本 | 不做（D-08 v1.1）| DeferredBadge；sidecar 子方案文档先行 |
| 5 | 识别管线预处理/后处理 | 蓝本 §3.10 | 后处理✅ 预处理❌ | 灰度/2x 放大/去噪（image crate 纯 Rust）+ 倾斜校正登记评估 |
| 6 | 段落合并、排除水印 | 蓝本 | 合并✅ 水印❌ | 设置开关合并策略；"排除水印"=低置信+边缘位置启发，登记待评估 |
| 7 | 带位置文本块/置信度 | 蓝本 OcrResult | ✅ DTO 孤儿 | 双栏对照 UI |
| 8 | 引擎切换 UI | 蓝本"用户可切换" | ❌ | 引擎子面板 |
| 9 | 与截图联动 OCR 按钮 | 蓝本 | ✅ 事件链 | 保留；langs 注入修复 |
| 10 | 离线批量图 OCR | Umi-OCR 招牌 | ❌ | 批量子面板 |
| 11 | GUI/PDF 识别 | Umi-OCR | ❌ | PDF：lopdf 抽内嵌文本成功即直返，失败页位图渲染再识别（渲染依赖登记评估，v1 可先"截图该页"引导）|
| 12 | 二维码/条形码 | Umi-OCR | ❌ | 纯 Rust 解码 crate，识别子面板前置分支 |
| 13 | 翻译接口位 | Umi-OCR | 不做（D-08）| 结果区留按钮位挂 DeferredBadge |

## 4. 其他软件借鉴

- **Umi-OCR**：排版合并开关粒度（单行/段落/禁合并三态）照搬；"忽略区域"圈选（先涂掉水印/字幕再识别）作拓展③；批量任务 csv 含 bbox 列的导出格式。
- **eSearch**：截图后**就地**框选结果词翻译/复制的即时感——本仓无翻译则做"就地搜词"：选中结果词→一键送宿主搜索（desktop 模块启动器带词打开搜索）。
- **Textract**：多页文档"整档合并成一份 md"导出（含图片占位引用）——服务笔记模块的扫描导入流。
- **PowerToys Advanced Paste**：Windows 已有 Paste-as-OCR 心智——快捷键"粘贴为文本"：剪贴板是图时自动走识别→文本入栈（clipboard 模块收这条边）。

## 5. 拓展设计（常人未思）

1. **识别即卡片**：结果块可直接"转为笔记/转为待办/创建记忆卡"（notes_card_create 现成）——截图里的会议白板 30 秒后进复习队列，OCR 与间隔重复的桥没人搭过。
2. **引擎 A/B 对照**：同图并发跑 WinOCR+Tesseract，左右分栏 diff 置信度——既是调试器也是选型器，还反向沉淀"何时该换引擎"的用户直觉。
3. **忽略区域**：识别前在同一画布上涂抹排除区（水印/进度条），矩形掩膜在预处理阶段抹白——Umi-OCR 有、Win 生态几乎没有。
4. **低置信守护红线**：疑似密钥/密码样式（高熵短串）的识别结果默认打码显示，显式点击才展开——OCR 历史与批量导出不得静默囤积敏感明文（D-04 信封同族处置：入历史即评估敏感标记）。
5. **语言自动嗅探**：pick 前对图内文本区做笔画密度/字符集启发（CJK 占比）自动填 langs，用户只校对不改——把"恒空"的病根反转成"默认就对"。
6. **常白名单**：批量任务失败分类重试（损坏图/过大/无文本），"无文本"单独归档而不是报错——扫描工作流的真实噪声形状。

## 6. 验收点

- B0：ocr 有面板注册（MODULES vitest 断言）、`ocr_engine_status` 与 `set_preferred` 端到端（改首选后 pick 结果变化断言）；
- langs 注入回归：配置中文优先 → OverlayShot 链路 pick 收到非空 langs（负例：空配置仍走引擎默认不报错）；
- Tesseract 引擎：注册顺序=优先级单测 + available=false 降级错误码断言（不静默空结果）；
- 批量：千图目录队列内存曲线（流式逐图，禁整目录入内存）；敏感打码负例（高熵串在历史/导出中均为打码态）；
- 二维码：标准样本图正例 + 模糊图失败降级走文字识别。
