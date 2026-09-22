import { useCallback, useEffect, useRef, useState } from "react";
import { makeStyles, tokens, Button, Slider, Switch, Text } from "@fluentui/react-components";
import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import {
  screenshotTask,
  screenshotConfirm,
  screenshotFinish,
  ocrRecognize,
  hostLog,
  type TaskStartDto,
  type TaskInfoDto,
  type CropDto,
  type OcrResultDto,
  type AnnotationDto,
} from "../ipc/client";
import { reportError } from "../stores/notifications";
import { confirmAction, type ConfirmOptions } from "../stores/confirm";
import InlineError from "../components/InlineError";
import { cancelOverlay } from "./overlayController";
import {
  ANN_KIND_NAME,
  HIGHLIGHT_WIDTH_SCALE,
  LAYER_OP_LABEL,
  TOOL_ABBR,
  TOOL_KINDS,
  boundsOf,
  finishTextDraft,
  hitTest,
  honoursFill,
  layerRows,
  moveLayer,
  removeAt,
  sortByLayer,
  stampNewAnn,
  strokeAlphaOf,
  toggleLock,
  translate,
  type Ann,
  type AnnKind,
  type LayerOp,
  type TextDraft,
  type Tool,
} from "./overlay/annotations";
import { applyBoxBlurPass } from "./overlay/pixel";

/**
 * 截图覆盖层（docs/impl/03 P3 选区 + P4 标注 + P5 动作 + docs/impl/04 O7 结果面板）。
 *
 * 两阶段：
 *  select —— 全屏暗化 + 拖拽选区（物理坐标 = CSS 坐标 × task.width/innerWidth，
 *            对窗口缩放系数不敏感）；Enter/双击确认，Esc 取消。
 *  edit   —— 裁剪图 + canvas 标注（预览即合成，finish 直接导出 canvas 数据，
 *            保证导出与预览一致）；OCR 走 ocr_recognize，结果面板可复制。
 *
 * mode=ocr（Ctrl+Alt+O 触发）：确认选区后自动执行 OCR。
 */
const useStyles = makeStyles({
  root: {
    position: "fixed",
    inset: 0,
    overflow: "hidden",
    userSelect: "none",
    backgroundColor: "#000",
  },
  backdrop: {
    position: "absolute",
    inset: 0,
    backgroundSize: "100% 100%",
    imageRendering: "auto",
  },
  dim: {
    position: "absolute",
    inset: 0,
    backgroundColor: "rgba(0,0,0,0.45)",
  },
  selection: {
    position: "absolute",
    border: "1px solid #ffffffcc",
    boxShadow: "0 0 0 1px rgba(0,0,0,0.6)",
    // 用超大 box-shadow 把选区外区域"抠亮"（比四块遮罩少 3 个元素）
    outline: "9999px solid rgba(0,0,0,0.55)",
    cursor: "move",
  },
  hint: {
    position: "absolute",
    top: "18px",
    left: "50%",
    transform: "translateX(-50%)",
    padding: "6px 14px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: "rgba(28,28,30,0.92)",
    color: "#fff",
    fontSize: tokens.fontSizeBase200,
    pointerEvents: "none",
  },
  sizeBadge: {
    position: "absolute",
    padding: "2px 8px",
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: "rgba(28,28,30,0.92)",
    color: "#fff",
    fontSize: tokens.fontSizeBase100,
    pointerEvents: "none",
  },
  editRoot: {
    position: "absolute",
    inset: 0,
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "10px",
    backgroundColor: "rgba(20,20,22,0.92)",
  },
  canvasBox: {
    position: "relative",
    maxWidth: "86vw",
    maxHeight: "76vh",
    display: "flex",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    boxShadow: "0 8px 30px rgba(0,0,0,0.6)",
  },
  toolbar: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "6px 10px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground3,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    flexWrap: "wrap",
    maxWidth: "90vw",
  },
  tool: {
    minWidth: "36px",
    justifyContent: "center",
  },
  activeTool: {
    minWidth: "36px",
    justifyContent: "center",
    backgroundColor: tokens.colorBrandBackground,
    color: tokens.colorNeutralForegroundOnBrand,
  },
  colorDot: {
    width: "20px",
    height: "20px",
    borderRadius: "50%",
    border: "2px solid transparent",
    cursor: "pointer",
    padding: 0,
  },
  activeColor: {
    width: "20px",
    height: "20px",
    borderRadius: "50%",
    border: `2px solid ${tokens.colorBrandForeground1}`,
    cursor: "pointer",
    padding: 0,
  },
  ocrPanel: {
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    width: "min(760px, 86vw)",
    maxHeight: "24vh",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  ocrText: {
    overflowY: "auto",
    whiteSpace: "pre-wrap",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground1,
    userSelect: "text",
  },
  ocrHead: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
  },
  // 操作错误条外壳：红字本体由 InlineError 提供（D-18 错误样式去重）
  errBar: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    padding: "6px 12px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorPaletteRedBorder1}`,
    maxWidth: "86vw",
  },
  // 图层侧栏（T-B4-1）：与画布并排，行=最上层在首行
  editRow: {
    display: "flex",
    alignItems: "flex-start",
    gap: "10px",
  },
  layerPanel: {
    display: "flex",
    flexDirection: "column",
    gap: "4px",
    width: "240px",
    maxHeight: "76vh",
    overflowY: "auto",
    padding: "8px",
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  layerRow: {
    display: "flex",
    alignItems: "center",
    gap: "4px",
    padding: "2px 4px",
    borderRadius: tokens.borderRadiusMedium,
    border: "1px solid transparent",
    cursor: "pointer",
  },
  layerRowSelected: {
    display: "flex",
    alignItems: "center",
    gap: "4px",
    padding: "2px 4px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorBrandStroke1}`,
    backgroundColor: tokens.colorNeutralBackground3,
    cursor: "pointer",
  },
  layerSwatch: {
    width: "12px",
    height: "12px",
    flexShrink: 0,
    borderRadius: "3px",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
  },
  /** 内联文字输入框：绝对定位在点击处（canvasBox 已是 relative 容器） */
  textInput: {
    position: "absolute",
    minWidth: "140px",
    userSelect: "text",
    fontSize: "14px",
    padding: "1px 4px",
    backgroundColor: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    border: `1px solid ${tokens.colorBrandStroke1}`,
    borderRadius: tokens.borderRadiusSmall,
  },
  /** 自定义色：原生 `<input type="color">` 只留一个色块那么大的可点区 */
  colorInput: {
    width: "22px",
    height: "22px",
    padding: 0,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: "3px",
    background: "transparent",
  },
  alphaCell: {
    display: "flex",
    alignItems: "center",
    width: "120px",
  },
});

type Stage = "select" | "edit";

const COLORS = ["#ff4d4f", "#ffb020", "#52c41a", "#1677ff", "#ffffff"];
const WIDTHS = [2, 4, 8];

/** CSS 坐标 → 抓帧物理坐标（窗口铺满虚拟桌面，比例恒定） */
function cssToPhysical(css: number, physical: number, view: number): number {
  if (view <= 0) return 0;
  return Math.round((css * physical) / view);
}

/** canvas 像素 → 容器内 CSS 偏移（`toCanvas` 的逆变换）：内联文字输入框要落在点击处 */
function canvasToCss(
  pt: { x: number; y: number },
  canvas: HTMLCanvasElement | null,
): { left: number; top: number } {
  const r = canvas?.getBoundingClientRect();
  if (!canvas || !r || r.width <= 0 || r.height <= 0) return { left: pt.x, top: pt.y };
  return { left: (pt.x * r.width) / canvas.width, top: (pt.y * r.height) / canvas.height };
}

function b64ToUrl(b64: string): string {
  return `data:image/png;base64,${b64}`;
}

function dataUrlToB64(url: string): string {
  const idx = url.indexOf(",");
  return idx >= 0 ? url.slice(idx + 1) : url;
}

/** 折线描边（pen / highlight 共用）：单点也留可见痕迹，否则单击成笔等于什么都不画 */
function strokePath(ctx: CanvasRenderingContext2D, pts: [number, number][]) {
  if (pts.length === 0) return;
  ctx.beginPath();
  ctx.moveTo(pts[0][0], pts[0][1]);
  for (let i = 1; i < pts.length; i++) ctx.lineTo(pts[i][0], pts[i][1]);
  if (pts.length === 1) ctx.lineTo(pts[0][0] + 0.01, pts[0][1]);
  ctx.stroke();
}

/** 错误规范化：Tauri invoke 抛的是对象，String(e) 会显示成 "[object Object]" */
function fmtErr(e: unknown): string {
  if (e && typeof e === "object") {
    const dto = e as { data?: { message?: string; code?: string } };
    if (dto.data?.message) return `${dto.data.message} (${dto.data.code ?? ""})`;
    return JSON.stringify(e);
  }
  return String(e);
}

export default function OverlayShot() {
  const styles = useStyles();
  const [task, setTask] = useState<TaskInfoDto | null>(null);
  const [stage, setStage] = useState<Stage>("select");
  const [crop, setCrop] = useState<CropDto | null>(null);
  /** 致命错误（任务加载失败）：替换整页 */
  const [error, setError] = useState<string | null>(null);
  /** 操作错误（复制/保存/OCR 等）：编辑页内错误条，不破坏界面 */
  const [actionError, setActionError] = useState<string | null>(null);
  /** 动作执行中（防连点 + 处理中反馈） */
  const [busy, setBusy] = useState(false);
  const [ocrResult, setOcrResult] = useState<OcrResultDto | null>(null);
  const [ocrBusy, setOcrBusy] = useState(false);
  const [tool, setTool] = useState<Tool>("rect");
  const [color, setColor] = useState(COLORS[3]);
  const [strokeWidth, setStrokeWidth] = useState(4);
  /** 笔画透明度（样式条 Slider 的取值区间 0.1–1.0：给不到 0，全透明的笔画等于看不见） */
  const [strokeAlpha, setStrokeAlpha] = useState(1);
  /** 形状类是否实心（开关对非形状工具禁用，见样式条） */
  const [fillShape, setFillShape] = useState(false);
  /** 文字内联编辑草稿（null = 无进行中的输入）：取代原生弹窗式输入 */
  const [textDraft, setTextDraft] = useState<TextDraft | null>(null);
  /**
   * 草稿的"未落定"凭据。Enter 提交后往往紧跟一次 blur，两个入口都通向落定，
   * 只有先取走草稿的那个会真的提交，后到的取到 null 即返回。
   */
  const textDraftRef = useRef<TextDraft | null>(null);
  /** 内联编辑中：Esc/Enter 归输入框，全局快捷键让位（判据只看"有没有草稿"，与内容无关） */
  const textEditing = textDraft !== null;

  // 选区（CSS 像素）
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const [rect, setRect] = useState<{ x: number; y: number; w: number; h: number } | null>(null);

  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const baseImg = useRef<HTMLImageElement | null>(null);
  /** 已提交标注的离屏合成画布：提交增量写入，undo/redo 全量重放 */
  const committedRef = useRef<HTMLCanvasElement | null>(null);
  /** 标注状态机（docs/impl/03 P4）：undo = 弹出末条 → 重放；redo = 反向压回 → 重放 */
  const annsRef = useRef<Ann[]>([]);
  const redoRef = useRef<Ann[]>([]);
  /** 图层面板的可渲染快照：唯一写入点是 syncStackCounts（真源仍是 annsRef，避免双写漂移） */
  const [layerList, setLayerList] = useState<Ann[]>([]);
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null);
  /** select 工具按下时定格的位移基线（拖动过程中不回写基线，否则误差累积） */
  const dragBase = useRef<{ index: number; from: { x: number; y: number }; src: Ann } | null>(null);
  const drawing = useRef(false);
  const startPoint = useRef<{ x: number; y: number } | null>(null);
  /** 笔画中上一采样点（pen/mosaic 段式绘制） */
  const lastPoint = useRef<{ x: number; y: number } | null>(null);
  /** 当前笔画路径（mousedown 起点 + mousemove 采样） */
  const pathRef = useRef<[number, number][]>([]);
  /** 笔画开始时定格的样式（拖拽中改工具栏不影响进行中的笔画） */
  const strokeStyle = useRef({ color: "#1677ff", width: 4, alpha: 1, fill: false });
  /** 撤销/重做栈深度（state：驱动按钮 disabled，避免 ref 不触发渲染） */
  const [stackCounts, setStackCounts] = useState({ undo: 0, redo: 0 });

  /** 装载任务（预热路径）：定位窗口 → 取帧 → 重置状态 → 渲染完成后自显 */
  const loadTask = useCallback(async (info: TaskStartDto) => {
    try {
      const win = getCurrentWindow();
      // 物理像素显式定位（多显示器/高 DPI 下与抓帧坐标系一致）
      await win.setPosition(new PhysicalPosition(info.x, info.y));
      await win.setSize(new PhysicalSize(info.width, info.height));
      const t: TaskInfoDto = await screenshotTask(info.task_id);
      // 重置上一任务残留状态（预热窗口复用，组件不重新 mount）
      annsRef.current = [];
      redoRef.current = [];
      setLayerList([]);
      setSelectedIdx(null);
      textDraftRef.current = null;
      setTextDraft(null);
      setStackCounts({ undo: 0, redo: 0 });
      setError(null);
      setActionError(null);
      setOcrResult(null);
      setCrop(null);
      setRect(null);
      setAnchor(null);
      setStage("select");
      setTask(t);
      // 等背景帧渲染完成后才显示，避免黑帧闪烁
      await new Promise<void>((r) => requestAnimationFrame(() => requestAnimationFrame(() => r())));
      await win.show();
      await win.setFocus();
      hostLog("info", `overlay: 任务 ${info.task_id} 就绪显示`);
    } catch (e) {
      const msg = fmtErr(e);
      hostLog("error", `loadTask 失败: ${msg}`);
      setError(msg);
      await getCurrentWindow()
        .show()
        .catch((e2) =>
          reportError(e2, { context: "覆盖层错误页展示失败", dedupeKey: "overlay-show", toast: false }),
        ); // 出错也要展示错误页
    }
  }, []);

  // 任务装载：URL 参数（回退路径）+ nf:overlay:task 事件（预热路径）
  useEffect(() => {
    const taskId = new URLSearchParams(window.location.search).get("task");
    if (taskId) {
      screenshotTask(taskId)
        .then((t) => setTask(t))
        .catch((e) => setError(fmtErr(e)));
    }
    let unlisten: (() => void) | null = null;
    // StrictMode 双挂载：迟到监听器立即移除（与 MainWorkbench 同款防护）
    let cancelled = false;
    void listen<TaskStartDto>("nf:overlay:task", (e) => {
      void loadTask(e.payload);
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadTask]);

  /** 完成/取消后隐藏自身（保留预热窗口，下次热键秒开） */
  const closeSelf = useCallback(() => {
    void getCurrentWindow().hide();
  }, []);

  const cancel = useCallback(() => {
    const taskId = task?.task_id ?? new URLSearchParams(window.location.search).get("task");
    void cancelOverlay(taskId);
  }, [task]);

  /** 确认框在开：对话框的 Esc/Enter 会同时冒泡到本窗全局快捷键，须让位 */
  const confirmPending = useRef(false);
  const askConfirm = useCallback(async (opts: ConfirmOptions) => {
    confirmPending.current = true;
    try {
      return await confirmAction(opts);
    } finally {
      confirmPending.current = false;
    }
  }, []);

  /**
   * 回选区（「重选」按钮与 edit 阶段 Esc 共用，D-18）：有未导出标注时先经全局
   * 确认框点名影响面；无标注即刻回退（空手退出覆盖层不该被打断）。
   */
  const discardToSelect = useCallback(async () => {
    const anns = annsRef.current;
    if (anns.length > 0) {
      const byKind = new Map<AnnKind, number>();
      for (const a of anns) byKind.set(a.kind, (byKind.get(a.kind) ?? 0) + 1);
      const impact = [
        `将丢弃未导出的标注 ${anns.length} 处：${[...byKind]
          .map(([k, n]) => `${ANN_KIND_NAME[k]} ${n}`)
          .join("、")}`,
      ];
      if (redoRef.current.length > 0) impact.push(`已撤销待重做 ${redoRef.current.length} 处`);
      if (crop) impact.push(`当前裁剪图 ${crop.width}×${crop.height} 与选区一并丢弃`);
      if (
        !(await askConfirm({
          title: "放弃当前标注并重新选区",
          impact,
          detail:
            "标注只活在本页的合成预览里：未点「完成/复制/保存/贴图」导出就不会进截图历史，放弃后无法恢复。",
          confirmLabel: "放弃标注",
        }))
      )
        return;
    }
    annsRef.current = [];
    redoRef.current = [];
    setLayerList([]);
    setSelectedIdx(null);
    textDraftRef.current = null;
    setTextDraft(null);
    setStackCounts({ undo: 0, redo: 0 });
    setStage("select");
    setRect(null);
  }, [askConfirm, crop]);

  // Esc 取消（select 阶段直接关窗；edit 阶段回选区，有标注则先确认）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (confirmPending.current) return;
      // 内联文字编辑中：本窗全局键（Esc/Enter/Ctrl+Z…）一律让位给输入框
      if (textEditing) return;
      if (e.key === "Escape") {
        e.preventDefault();
        if (stage === "edit") void discardToSelect();
        else cancel();
      }
      if (e.key === "Enter" && stage === "select" && rect && rect.w > 4 && rect.h > 4) {
        e.preventDefault();
        void confirmSelection();
      }
      // 标注撤销/重做快捷键（edit 阶段）
      if (stage === "edit") {
        const mod = e.ctrlKey || e.metaKey;
        if (mod && e.key.toLowerCase() === "z") {
          e.preventDefault();
          if (e.shiftKey) redo();
          else undo();
        }
        if (mod && e.key.toLowerCase() === "y") {
          e.preventDefault();
          redo();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // undo/redo/confirmSelection 为 ref-only 普通函数（每次渲染新身份），入依赖表
    // 会导致每帧重挂监听且行为不变；stage/rect/cancel 已在表内保证语义快照
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stage, rect, cancel, discardToSelect, textEditing]);

  /** 选区确认：物理坐标裁剪 → 进入编辑阶段（ocr 模式自动识别） */
  const confirmSelection = useCallback(async () => {
    if (!task || !rect) return;
    const viewW = window.innerWidth;
    const viewH = window.innerHeight;
    const physical = {
      x: cssToPhysical(rect.x, task.width, viewW),
      y: cssToPhysical(rect.y, task.height, viewH),
      w: cssToPhysical(rect.w, task.width, viewW),
      h: cssToPhysical(rect.h, task.height, viewH),
    };
    try {
      const c = await screenshotConfirm(task.task_id, physical);
      setCrop(c);
      setStage("edit");
      if (task.mode === "ocr") {
        // ocr 模式：裁剪即识别（不需要标注）
        void runOcr(c);
      }
    } catch (e) {
      setError(fmtErr(e));
    }
    // runOcr 是本回调之后的 const（TDZ 无法入依赖表）；其只读 ref/setter，快照无害
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [task, rect]);

  // ---------------- 编辑阶段：canvas 标注（docs/impl/03 P4 状态机）----------------

  /** 裁剪图加载到 canvas（一次性） */
  const setupCanvas = useCallback((c: CropDto) => {
    const img = new Image();
    img.onload = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      canvas.width = c.width;
      canvas.height = c.height;
      // 离屏"已提交"画布：与可见画布同尺寸
      const committed = committedRef.current ?? document.createElement("canvas");
      committed.width = c.width;
      committed.height = c.height;
      committedRef.current = committed;
      baseImg.current = img;
      const ctx = canvas.getContext("2d");
      ctx?.drawImage(img, 0, 0);
      committed.getContext("2d")?.drawImage(img, 0, 0);
      annsRef.current = [];
      redoRef.current = [];
      setLayerList([]);
      setSelectedIdx(null);
      textDraftRef.current = null;
      setTextDraft(null);
      setStackCounts({ undo: 0, redo: 0 });
    };
    img.src = b64ToUrl(c.png_b64);
  }, []);

  useEffect(() => {
    if (crop && stage === "edit") setupCanvas(crop);
  }, [crop, stage, setupCanvas]);

  /** CSS → canvas 像素坐标 */
  const toCanvas = (e: { clientX: number; clientY: number }) => {
    const canvas = canvasRef.current!;
    const r = canvas.getBoundingClientRect();
    const sx = canvas.width / r.width;
    const sy = canvas.height / r.height;
    return {
      x: (e.clientX - r.left) * sx,
      y: (e.clientY - r.top) * sy,
      sx,
      sy,
    };
  };

  /** 马赛克：from→to 线段上按 12px 步进做块均值化（确定性操作，可重放） */
  const applyMosaicPair = (
    ctx: CanvasRenderingContext2D,
    from: { x: number; y: number },
    to: { x: number; y: number },
    radius: number,
  ) => {
    const canvas = ctx.canvas;
    const block = 12;
    const dist = Math.hypot(to.x - from.x, to.y - from.y);
    const steps = Math.max(1, Math.ceil(dist / (block / 2)));
    for (let s = 0; s <= steps; s++) {
      const cx = from.x + ((to.x - from.x) * s) / steps;
      const cy = from.y + ((to.y - from.y) * s) / steps;
      const x0 = Math.max(0, Math.floor(cx - radius));
      const y0 = Math.max(0, Math.floor(cy - radius));
      const w = Math.min(canvas.width - x0, radius * 2);
      const h = Math.min(canvas.height - y0, radius * 2);
      if (w <= 0 || h <= 0) continue;
      const data = ctx.getImageData(x0, y0, w, h);
      const d = data.data;
      let r = 0;
      let g = 0;
      let b = 0;
      for (let i = 0; i < d.length; i += 4) {
        r += d[i];
        g += d[i + 1];
        b += d[i + 2];
      }
      const n = d.length / 4;
      const avg = [r / n, g / n, b / n];
      for (let i = 0; i < d.length; i += 4) {
        d[i] = avg[0];
        d[i + 1] = avg[1];
        d[i + 2] = avg[2];
      }
      ctx.putImageData(data, x0, y0);
    }
  };

  /** 单条标注绘制（参数化样式；重放与提交共用，保证一致性） */
  const applyAnn = (ctx: CanvasRenderingContext2D, ann: AnnotationDto) => {
    ctx.strokeStyle = ann.color;
    ctx.fillStyle = ann.color;
    ctx.lineWidth = ann.width;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    // 高亮在此处就被钳到 HIGHLIGHT_ALPHA_MAX：预览、提交、重放走的是同一个数
    ctx.globalAlpha = strokeAlphaOf(ann.kind, ann.alpha ?? 1);
    const fill = ann.fill === true;
    const pts = ann.points;
    switch (ann.kind) {
      case "pen": {
        strokePath(ctx, pts);
        break;
      }
      case "highlight": {
        // 荧光笔与画笔的全部区别就是这两处常量：宽 ×4、平头（首尾不鼓包），加上钳位透明度
        ctx.lineCap = "butt";
        ctx.lineWidth = ann.width * HIGHLIGHT_WIDTH_SCALE;
        strokePath(ctx, pts);
        break;
      }
      case "line": {
        if (pts.length < 2) break;
        ctx.beginPath();
        ctx.moveTo(pts[0][0], pts[0][1]);
        ctx.lineTo(pts[1][0], pts[1][1]);
        ctx.stroke();
        break;
      }
      case "rect": {
        if (pts.length < 2) break;
        const x = Math.min(pts[0][0], pts[1][0]);
        const y = Math.min(pts[0][1], pts[1][1]);
        const w = Math.abs(pts[1][0] - pts[0][0]);
        const h = Math.abs(pts[1][1] - pts[0][1]);
        if (fill) ctx.fillRect(x, y, w, h);
        else ctx.strokeRect(x, y, w, h);
        break;
      }
      case "ellipse": {
        if (pts.length < 2) break;
        ctx.beginPath();
        ctx.ellipse(
          (pts[0][0] + pts[1][0]) / 2,
          (pts[0][1] + pts[1][1]) / 2,
          Math.abs(pts[1][0] - pts[0][0]) / 2,
          Math.abs(pts[1][1] - pts[0][1]) / 2,
          0,
          0,
          Math.PI * 2,
        );
        if (fill) ctx.fill();
        else ctx.stroke();
        break;
      }
      case "blur": {
        if (pts.length < 2) break;
        const x0 = Math.max(0, Math.round(Math.min(pts[0][0], pts[1][0])));
        const y0 = Math.max(0, Math.round(Math.min(pts[0][1], pts[1][1])));
        const w = Math.min(ctx.canvas.width - x0, Math.round(Math.abs(pts[1][0] - pts[0][0])));
        const h = Math.min(ctx.canvas.height - y0, Math.round(Math.abs(pts[1][1] - pts[0][1])));
        if (w <= 0 || h <= 0) break;
        const data = ctx.getImageData(x0, y0, w, h);
        applyBoxBlurPass(data, Math.max(2, Math.round(ann.width * 2)));
        ctx.putImageData(data, x0, y0);
        break;
      }
      case "arrow": {
        if (pts.length < 2) break;
        const [fx, fy] = pts[0];
        const [tx, ty] = pts[1];
        const ang = Math.atan2(ty - fy, tx - fx);
        const head = Math.max(10, ann.width * 4);
        ctx.beginPath();
        ctx.moveTo(fx, fy);
        ctx.lineTo(tx, ty);
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(tx, ty);
        ctx.lineTo(tx - head * Math.cos(ang - Math.PI / 6), ty - head * Math.sin(ang - Math.PI / 6));
        ctx.lineTo(tx - head * Math.cos(ang + Math.PI / 6), ty - head * Math.sin(ang + Math.PI / 6));
        ctx.closePath();
        ctx.fill();
        break;
      }
      case "text": {
        if (!ann.text || pts.length < 1) break;
        ctx.font = `bold ${ann.width * 6 + 8}px "Segoe UI", sans-serif`;
        // 序号绘制把 textAlign 改成了 center，不在此复位就会串到后画的文字上（重放序无关性）
        ctx.textAlign = "left";
        ctx.textBaseline = "top";
        ctx.fillText(ann.text, pts[0][0], pts[0][1]);
        break;
      }
      case "number": {
        if (pts.length < 1) break;
        const r = 14;
        const [cx, cy] = pts[0];
        ctx.beginPath();
        ctx.arc(cx, cy, r, 0, Math.PI * 2);
        ctx.fill();
        ctx.fillStyle = "#ffffff";
        ctx.font = `bold ${r + 2}px "Segoe UI", sans-serif`;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(String(ann.seq ?? 1), cx, cy + 1);
        break;
      }
      case "mosaic": {
        // 马赛克按路径逐段重放（依赖当时画布像素，顺序不可变）
        const radius = Math.max(6, ann.width * 3);
        for (let i = 1; i < pts.length; i++) {
          applyMosaicPair(
            ctx,
            { x: pts[i - 1][0], y: pts[i - 1][1] },
            { x: pts[i][0], y: pts[i][1] },
            radius,
          );
        }
        break;
      }
    }
    ctx.globalAlpha = 1;
  };

  /** 全量重放：离屏画布 = 底图 + 全部标注（按 layer 升序），再同步到可见画布 */
  const replayAll = (list: Ann[]) => {
    const committed = committedRef.current;
    const canvas = canvasRef.current;
    if (!committed || !canvas || !baseImg.current) return;
    const cctx = committed.getContext("2d");
    const vctx = canvas.getContext("2d");
    if (!cctx || !vctx) return;
    cctx.clearRect(0, 0, committed.width, committed.height);
    cctx.drawImage(baseImg.current, 0, 0);
    // 锁定项照旧绘制：锁定只作用于点选穿透，不改变画面（图层面板的"锁定"不是隐藏）
    for (const ann of sortByLayer(list)) applyAnn(cctx, ann);
    vctx.clearRect(0, 0, canvas.width, canvas.height);
    vctx.drawImage(committed, 0, 0);
  };

  /** 栈与面板的唯一同步出口：任何改动 annsRef 的路径都必须过这里 */
  const syncStackCounts = () => {
    setStackCounts({ undo: annsRef.current.length, redo: redoRef.current.length });
    setLayerList(annsRef.current.slice());
    const n = annsRef.current.length;
    setSelectedIdx((cur) => (cur === null || cur >= n ? null : cur));
  };

  /**
   * 提交一条标注：增量写入离屏画布 + 入撤销栈 + 清空重做栈。
   * layer/alpha/fill 在 stampNewAnn（纯模块）里定格，本处只负责画与入栈——
   * 高亮的钳位透明度因此"提交的就是显示的那个数"，历史读回来重画也是同一个数。
   */
  const commitAnn = (draft: AnnotationDto) => {
    const committed = committedRef.current;
    const canvas = canvasRef.current;
    if (!committed || !canvas) return;
    const cctx = committed.getContext("2d");
    if (!cctx) return;
    const ann = stampNewAnn(annsRef.current, draft);
    applyAnn(cctx, ann);
    annsRef.current.push(ann);
    redoRef.current = [];
    // 可见画布与离屏对齐（笔画预览期间可能残留中间态）
    const vctx = canvas.getContext("2d");
    vctx?.clearRect(0, 0, canvas.width, canvas.height);
    vctx?.drawImage(committed, 0, 0);
    syncStackCounts();
  };

  /**
   * 内联文字落定：commit（Enter / 失焦）与 cancel（Esc）都先取走草稿凭据再决定是否画。
   * "要不要产生一条标注"由纯函数 finishTextDraft 裁决（空值与纯空白同样零标注零栈条目）。
   */
  const settleText = (action: "commit" | "cancel") => {
    const draft = textDraftRef.current;
    if (!draft) return;
    textDraftRef.current = null;
    setTextDraft(null);
    const done = finishTextDraft(draft, action, { color, width: strokeWidth });
    if (done) commitAnn(done);
  };

  const undo = () => {
    const last = annsRef.current.pop();
    if (!last) return;
    redoRef.current.push(last);
    replayAll(annsRef.current);
    syncStackCounts();
  };

  const redo = () => {
    const next = redoRef.current.pop();
    if (!next) return;
    annsRef.current.push(next);
    replayAll(annsRef.current);
    syncStackCounts();
  };

  /** 图层面板四个行操作：全部走纯模块函数，再全量重放（z 序变了就不能增量画） */
  const applyLayerOp = (index: number, op: LayerOp) => {
    const list = annsRef.current;
    if (index < 0 || index >= list.length) return;
    if (op === "delete") {
      annsRef.current = removeAt(list, index);
      replayAll(annsRef.current);
      syncStackCounts();
      setSelectedIdx(null);
      return;
    }
    annsRef.current =
      op === "lock"
        ? toggleLock(list, index)
        : moveLayer(list, index, op === "up" ? 1 : -1);
    replayAll(annsRef.current);
    syncStackCounts();
  };

  /** 选中项描边：只画在可见画布上，不进离屏合成（导出图不含 UI 痕迹） */
  const drawSelectionOutline = () => {
    const canvas = canvasRef.current;
    const committed = committedRef.current;
    if (!canvas || !committed) return;
    const vctx = canvas.getContext("2d");
    if (!vctx) return;
    vctx.clearRect(0, 0, canvas.width, canvas.height);
    vctx.drawImage(committed, 0, 0);
    if (selectedIdx === null) return;
    const ann = annsRef.current[selectedIdx];
    const box = ann ? boundsOf(ann) : null;
    if (!box) return;
    vctx.save();
    vctx.setLineDash([6, 4]);
    vctx.strokeStyle = "#1677ff";
    vctx.lineWidth = 2;
    vctx.strokeRect(box.x - 2, box.y - 2, box.w + 4, box.h + 4);
    vctx.restore();
  };

  // 选中/图层变化后补描边（绘制路径本身只维护离屏，UI 痕迹一律留在可见画布这一层）
  useEffect(() => {
    drawSelectionOutline();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 依赖即"这两个变了才重描"
  }, [selectedIdx, layerList]);

  /** 实时预览：可见画布 = 已提交合成 + 进行中笔画（形状类每帧从离屏重绘） */
  const previewShape = (
    from: { x: number; y: number },
    to: { x: number; y: number },
    path?: [number, number][],
  ) => {
    const canvas = canvasRef.current;
    const committed = committedRef.current;
    if (!canvas || !committed) return;
    const vctx = canvas.getContext("2d");
    if (!vctx) return;
    vctx.clearRect(0, 0, canvas.width, canvas.height);
    vctx.drawImage(committed, 0, 0);
    const style = strokeStyle.current;
    // 序号预览用"下一个序号"（提交时定格为同值）
    const seq = annsRef.current.filter((a) => a.kind === "number").length + 1;
    applyAnn(vctx, {
      kind: tool === "number" ? "number" : (tool as AnnKind),
      color: style.color,
      width: style.width,
      alpha: style.alpha,
      fill: style.fill,
      points: path ?? [[from.x, from.y], [to.x, to.y]],
      seq,
    });
  };

  const onCanvasMouseDown = (e: React.MouseEvent) => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const p = toCanvas(e);
    strokeStyle.current = { color, width: strokeWidth, alpha: strokeAlpha, fill: fillShape };

    if (tool === "select") {
      // 点选：从最上层往下命中（锁定项穿透，但锁定项在图层面板里仍可选、仍可解锁）
      const hit = hitTest(annsRef.current, [p.x, p.y], [canvas.width, canvas.height], {
        skipLocked: true,
      });
      if (hit === null) {
        setSelectedIdx(null);
        return;
      }
      setSelectedIdx(hit);
      dragBase.current = { index: hit, from: { x: p.x, y: p.y }, src: annsRef.current[hit] };
      return;
    }
    if (tool === "text") {
      // 内联编辑：在点击处摆输入框，Enter/失焦提交、Esc 取消（取消与空值零标注零栈条目）
      const draft: TextDraft = { x: p.x, y: p.y, value: "" };
      textDraftRef.current = draft;
      setTextDraft(draft);
      return;
    }
    if (tool === "number") {
      // 单击生效（mouseup 提交）；按下仅记录起点
      drawing.current = true;
      startPoint.current = p;
      pathRef.current = [[p.x, p.y]];
      previewShape(p, p);
      return;
    }
    drawing.current = true;
    startPoint.current = p;
    lastPoint.current = p;
    pathRef.current = [[p.x, p.y]];
    if (tool === "pen" || tool === "mosaic") {
      // 起笔即画一段（可见画布；提交时以同路径写入离屏）
      const vctx = canvas.getContext("2d");
      if (vctx) {
        if (tool === "pen") {
          applyAnn(vctx, {
            kind: "pen",
            color,
            width: strokeWidth,
            alpha: strokeAlpha,
            points: [
              [p.x, p.y],
              [p.x + 0.01, p.y],
            ],
          });
        } else {
          applyMosaicPair(vctx, p, p, Math.max(6, strokeWidth * 3));
        }
      }
    } else {
      // 其余（直线/高亮/模糊/矩形/椭圆/箭头）：零尺寸预览即起点，后续帧由 move 驱动
      previewShape(p, p, pathRef.current);
    }
  };

  const onCanvasMouseMove = (e: React.MouseEvent) => {
    const base = dragBase.current;
    if (base) {
      // 拖动位移：每帧从按下时的基线重算（不在当前坐标上累加，否则误差会随帧累积）
      const cur = toCanvas(e);
      annsRef.current[base.index] = translate(base.src, cur.x - base.from.x, cur.y - base.from.y);
      replayAll(annsRef.current);
      return;
    }
    if (!drawing.current) return;
    const p = toCanvas(e);
    if (tool === "pen") {
      // pen 为加法绘制：增量画段即可，无需整幅重绘
      const vctx = canvasRef.current?.getContext("2d");
      const from = lastPoint.current;
      if (vctx && from) {
        applyAnn(vctx, {
          kind: "pen",
          color: strokeStyle.current.color,
          width: strokeStyle.current.width,
          alpha: strokeStyle.current.alpha,
          points: [
            [from.x, from.y],
            [p.x, p.y],
          ],
        });
      }
      pathRef.current.push([p.x, p.y]);
      lastPoint.current = p;
    } else if (tool === "mosaic") {
      const vctx = canvasRef.current?.getContext("2d");
      const from = lastPoint.current;
      if (vctx && from) {
        applyMosaicPair(vctx, from, p, Math.max(6, strokeStyle.current.width * 3));
      }
      pathRef.current.push([p.x, p.y]);
      lastPoint.current = p;
    } else if (tool === "highlight") {
      // 高亮每帧从合成重画整条路径：半透明笔刷逐段叠加会在接缝处显出深色节点
      pathRef.current.push([p.x, p.y]);
      lastPoint.current = p;
      previewShape(startPoint.current!, p, pathRef.current);
    } else {
      // rect/ellipse/line/arrow/blur：从离屏合成重绘 + 当前形状
      previewShape(startPoint.current!, p);
    }
  };

  const onCanvasMouseUp = (e: React.MouseEvent) => {
    const base = dragBase.current;
    if (base) {
      const cur = toCanvas(e);
      annsRef.current[base.index] = translate(base.src, cur.x - base.from.x, cur.y - base.from.y);
      dragBase.current = null;
      replayAll(annsRef.current);
      syncStackCounts();
      return;
    }
    if (!drawing.current) return;
    drawing.current = false;
    const p = toCanvas(e);
    const start = startPoint.current!;
    const style = strokeStyle.current;
    const twoPoint: [number, number][] = [
      [start.x, start.y],
      [p.x, p.y],
    ];
    switch (tool) {
      case "pen":
      case "mosaic":
      case "highlight":
        commitAnn({
          kind: tool,
          color: style.color,
          width: style.width,
          alpha: style.alpha,
          points: pathRef.current.length > 0 ? pathRef.current : twoPoint,
        });
        break;
      case "rect":
      case "ellipse":
        commitAnn({
          kind: tool,
          color: style.color,
          width: style.width,
          alpha: style.alpha,
          fill: style.fill,
          points: twoPoint,
        });
        break;
      case "line":
      case "arrow":
      case "blur":
        commitAnn({
          kind: tool,
          color: style.color,
          width: style.width,
          alpha: style.alpha,
          points: twoPoint,
        });
        break;
      case "number": {
        // 双击不会到这里（number 无拖拽）；单击位置定格
        commitAnn({
          kind: "number",
          color: style.color,
          width: style.width,
          alpha: style.alpha,
          points: [[p.x, p.y]],
          seq: annsRef.current.filter((a) => a.kind === "number").length + 1,
        });
        break;
      }
      default:
        break;
    }
    pathRef.current = [];
    lastPoint.current = null;
    startPoint.current = null;
  };

  /** 合成导出（预览即导出） */
  const compositeB64 = (): string | null => {
    const canvas = canvasRef.current;
    const committed = committedRef.current;
    if (!canvas || !committed) return null;
    // 导出前把可见画布重置为纯合成：选中描边是 UI 痕迹，不该进导出图
    const vctx = canvas.getContext("2d");
    vctx?.clearRect(0, 0, canvas.width, canvas.height);
    vctx?.drawImage(committed, 0, 0);
    return dataUrlToB64(canvas.toDataURL("image/png"));
  };

  // ---------------- 动作 ----------------

  const finish = async (actions: string[]) => {
    if (!task || busy) return;
    setBusy(true);
    try {
      const image = compositeB64();
      if (!image) return;
      try {
        const result = await screenshotFinish(task.task_id, {
          image_b64: image,
          actions,
          pin_x: null,
          pin_y: null,
          annotations: annsRef.current,
        });
        if (result.pin_id) {
          // 贴图窗口在主窗口恢复逻辑之外需要立即打开
          const { openPinWindow } = await import("./overlayController");
          const pw = crop?.width ?? 200;
          const ph = crop?.height ?? 200;
          // D-23：贴图居中于截图所在显示器（currentMonitor 给出该窗口的物理矩形；
          // crop 宽高本就是物理像素，无需再乘 devicePixelRatio——旧式 screen.width×dpr
          // 恒以主屏为基准，混合 DPI 双屏时副屏截图的贴图必落在错误显示器）
          const mon = await currentMonitor();
          const cx = mon
            ? mon.position.x + Math.round((mon.size.width - pw) / 2)
            : Math.round(((window.screen.width * (window.devicePixelRatio || 1)) - pw) / 2);
          const cy = mon
            ? mon.position.y + Math.round((mon.size.height - ph) / 2)
            : Math.round(((window.screen.height * (window.devicePixelRatio || 1)) - ph) / 2);
          await openPinWindow({
            id: result.pin_id,
            x: cx,
            y: cy,
            width: pw,
            height: ph,
            zoom: 1,
            opacity: 1,
          });
        }
        closeSelf();
      } catch (e) {
        const msg = fmtErr(e);
        hostLog("error", `finish(actions=${actions.join(",")}) 失败: ${msg}`);
        setActionError(msg);
      }
    } finally {
      setBusy(false);
    }
  };

  const runOcr = useCallback(async (c?: CropDto) => {
    const target = c ?? crop;
    if (!target || !task) return;
    setOcrBusy(true);
    setOcrResult(null);
    try {
      const result = await ocrRecognize({
        image_b64: target.png_b64,
        // 空数组 = 跟随设置里的偏好语言（后端 resolve_langs 单点解析），非"无偏好"
        langs: [],
        source_task_id: task.task_id,
      });
      setOcrResult(result);
    } catch (e) {
      const msg = fmtErr(e);
      hostLog("error", `ocr_recognize 失败: ${msg}`);
      setActionError(`OCR 失败: ${msg}`);
    } finally {
      setOcrBusy(false);
    }
  }, [crop, task]);

  // ---------------- 渲染 ----------------

  if (error) {
    return (
      <div className={styles.root} style={{ display: "grid", placeItems: "center" }}>
        <div style={{ textAlign: "center", color: "#fff" }}>
          <Text block>{error}</Text>
          <Button appearance="secondary" style={{ marginTop: 12 }} onClick={closeSelf}>
            关闭
          </Button>
        </div>
      </div>
    );
  }

  if (!task) {
    return <div className={styles.root} />;
  }

  if (stage === "select") {
    const selectionStyle = rect
      ? {
          left: rect.x,
          top: rect.y,
          width: rect.w,
          height: rect.h,
        }
      : undefined;
    return (
      <div
        className={styles.root}
        // 全屏取色框选面：纯绘图画布容器，交互语义由内部工具条按钮承担（jsx-a11y: presentation）
        role="presentation"
        style={{ cursor: "crosshair" }}
        onMouseDown={(e) => {
          if (e.button !== 0) return;
          setAnchor({ x: e.clientX, y: e.clientY });
          setRect({ x: e.clientX, y: e.clientY, w: 0, h: 0 });
        }}
        onMouseMove={(e) => {
          if (!anchor) return;
          setRect({
            x: Math.min(anchor.x, e.clientX),
            y: Math.min(anchor.y, e.clientY),
            w: Math.abs(e.clientX - anchor.x),
            h: Math.abs(e.clientY - anchor.y),
          });
        }}
        onMouseUp={() => setAnchor(null)}
        onDoubleClick={() => {
          if (rect && rect.w > 4 && rect.h > 4) void confirmSelection();
        }}
      >
        <div
          className={styles.backdrop}
          style={{ backgroundImage: `url(${b64ToUrl(task.png_b64)})` }}
        />
        {/* 无选区时整体暗化；有选区后由 selection 的超大 outline 负责抠亮 */}
        {!rect && <div className={styles.dim} />}
        {rect && rect.w > 2 && (
          <div className={styles.selection} style={selectionStyle}>
            <div
              className={styles.sizeBadge}
              style={{ left: "50%", top: "100%", transform: "translate(-50%, 8px)" }}
            >
              {Math.round((rect.w * task.width) / window.innerWidth)} ×{" "}
              {Math.round((rect.h * task.height) / window.innerHeight)}
            </div>
          </div>
        )}
        <div className={styles.hint}>
          拖拽选择区域 · 双击 / Enter 确认{task.mode === "ocr" ? "（自动 OCR）" : ""} · Esc 取消
        </div>
      </div>
    );
  }

  // edit 阶段
  return (
    <div className={styles.editRoot}>
      <div className={styles.toolbar}>
        <Button
          className={tool === "select" ? styles.activeTool : styles.tool}
          size="small"
          onClick={() => setTool("select")}
        >
          选
        </Button>
        {TOOL_KINDS.map((t) => (
          <Button
            key={t}
            className={tool === t ? styles.activeTool : styles.tool}
            size="small"
            title={ANN_KIND_NAME[t]}
            onClick={() => setTool(t)}
          >
            {TOOL_ABBR[t]}
          </Button>
        ))}
        <span style={{ width: 8 }} />
        {COLORS.map((c) => (
          <button
            key={c}
            className={color === c ? styles.activeColor : styles.colorDot}
            style={{ backgroundColor: c }}
            onClick={() => setColor(c)}
            aria-label={`颜色 ${c}`}
          />
        ))}
        <input
          type="color"
          className={styles.colorInput}
          value={color}
          onChange={(e) => setColor(e.target.value)}
          aria-label="自定义颜色"
          title="自定义颜色"
        />
        <span style={{ width: 8 }} />
        {WIDTHS.map((w) => (
          <Button
            key={w}
            className={strokeWidth === w ? styles.activeTool : styles.tool}
            size="small"
            onClick={() => setStrokeWidth(w)}
          >
            {w}
          </Button>
        ))}
        <span className={styles.alphaCell}>
          <Slider
            size="small"
            min={0.1}
            max={1}
            step={0.05}
            value={strokeAlpha}
            onChange={(_, d) => setStrokeAlpha(d.value)}
            aria-label={`透明度 ${strokeAlpha.toFixed(2)}`}
          />
        </span>
        <Switch
          checked={fillShape}
          disabled={!honoursFill(tool)}
          onChange={(_, d) => setFillShape(d.checked)}
          label="填充"
          size="small"
        />
        <span style={{ width: 8 }} />
        <Button size="small" className={styles.tool} onClick={undo} disabled={stackCounts.undo === 0}>
          ↶
        </Button>
        <Button size="small" className={styles.tool} onClick={redo} disabled={stackCounts.redo === 0}>
          ↷
        </Button>
      </div>

      <div className={styles.editRow}>
        <div className={styles.canvasBox}>
          <canvas
            ref={canvasRef}
            style={{ maxWidth: "86vw", maxHeight: "76vh" }}
            onMouseDown={onCanvasMouseDown}
            onMouseMove={onCanvasMouseMove}
            onMouseUp={onCanvasMouseUp}
          />
          {textDraft && (
            <input
              // 覆盖层是模态面：草稿存在的唯一目的就是立刻接住键盘，
              // 要用户多点一次才进入输入态反而更差（规则的一般性理由在此不成立）
              // eslint-disable-next-line jsx-a11y/no-autofocus
              autoFocus
              data-text-draft=""
              className={styles.textInput}
              style={canvasToCss(textDraft, canvasRef.current)}
              value={textDraft.value}
              onChange={(e) => {
                const next = { ...textDraft, value: e.target.value };
                textDraftRef.current = next;
                setTextDraft(next);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  settleText("commit");
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  settleText("cancel");
                }
              }}
              onBlur={() => settleText("commit")}
            />
          )}
        </div>
        {/* 图层面板（T-B4-1）：行模型出自纯模块 layerRows，本处只按行铺四钮 */}
        {layerList.length > 0 && (
          <div className={styles.layerPanel} data-layer-panel="">
            <Text weight="semibold" size={200}>
              图层（{layerList.length}）
            </Text>
            {layerRows(layerList).map((row) => (
              <div
                key={row.index}
                className={row.index === selectedIdx ? styles.layerRowSelected : styles.layerRow}
                // 行本身是选择器（画布上点不到的细标注由此可达）；键盘侧由四钮承担
                role="presentation"
                onClick={() => setSelectedIdx(row.index)}
              >
                <span className={styles.layerSwatch} style={{ backgroundColor: row.color }} />
                <Text size={200}>
                  {row.name}
                  {row.locked ? " 🔒" : ""}
                </Text>
                {row.ops.map((op) => (
                  <Button
                    key={op}
                    size="small"
                    className={styles.tool}
                    onClick={(ev) => {
                      ev.stopPropagation();
                      applyLayerOp(row.index, op);
                    }}
                  >
                    {op === "lock" && row.locked ? "解锁" : LAYER_OP_LABEL[op]}
                  </Button>
                ))}
              </div>
            ))}
          </div>
        )}
      </div>

      {(ocrBusy || ocrResult) && (
        <div className={styles.ocrPanel}>
          <div className={styles.ocrHead}>
            <Text weight="semibold" size={200}>
              {ocrBusy ? "识别中…" : `OCR 结果（${ocrResult?.engine ?? ""}${ocrResult?.lang ? " · " + ocrResult.lang : ""}）`}
            </Text>
            {ocrResult && (
              <>
                <Button
                  size="small"
                  onClick={() => {
                    if (ocrResult) void import("../ipc/client").then((m) => m.ocrCopyText(ocrResult.text));
                  }}
                >
                  复制全部
                </Button>
                <Button size="small" onClick={() => setOcrResult(null)}>
                  关闭
                </Button>
              </>
            )}
          </div>
          {ocrResult && <div className={styles.ocrText}>{ocrResult.text}</div>}
        </div>
      )}

      <div className={styles.toolbar}>
        <Button size="small" appearance="primary" onClick={() => void finish([])} disabled={busy}>
          {busy ? "处理中…" : "完成"}
        </Button>
        <Button size="small" onClick={() => void finish(["copy"])} disabled={busy}>
          复制
        </Button>
        <Button size="small" onClick={() => void finish(["save"])} disabled={busy}>
          保存
        </Button>
        <Button size="small" onClick={() => void finish(["pin"])} disabled={busy}>
          贴图
        </Button>
        <Button size="small" onClick={() => void runOcr()} disabled={ocrBusy || busy}>
          OCR
        </Button>
        <Button size="small" onClick={() => void discardToSelect()} disabled={busy}>
          重选
        </Button>
      </div>

      {actionError && (
        <div className={styles.errBar}>
          <InlineError text={actionError} />
          <Button size="small" onClick={() => setActionError(null)}>
            知道了
          </Button>
        </div>
      )}
    </div>
  );
}
