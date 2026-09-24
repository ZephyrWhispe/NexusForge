import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel from "../NotesPanel";
import {
  notesBacklinks,
  notesCanvasDirs,
  notesCanvasGet,
  notesCanvasSave,
  notesCards,
  notesList,
  notesLinks,
  notesRead,
  notesReviewQueue,
  notesReviewStats,
  notesSync,
  type CanvasDocDto,
  type NoteMetaDto,
} from "../../../ipc/client";
import { canvasImgSrc, deleteEdge, setNodeText } from "../canvasEdit";

// D-29 B7/T-B7-24：画布节点双击就地改文（存回 CanvasNode.text，写盘走既有 notesCanvasSave）
// + 单边选中删除（节点原样保留）+ 复习统计卡（notes_review_stats 四面之消费面）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesReviewStats: vi.fn(),
    notesCards: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesCanvasGet: vi.fn(),
    notesCanvasSave: vi.fn(async () => undefined),
    notesSearch: vi.fn(async () => []),
    notesSync: vi.fn(),
    notesByTag: vi.fn(),
    notesRead: vi.fn(),
    notesLinks: vi.fn(),
    notesBacklinks: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

// 约定（B7 第四次确认）：绝不直接 mock monaco-editor，用本地形制桩
vi.mock("../../../monaco/setup", () => {
  class FakeModel {
    private value = "";
    private disposed = false;
    constructor(private fire: () => void) {}
    isDisposed() {
      return this.disposed;
    }
    setValue(v: string) {
      this.value = v;
      this.fire();
    }
    getValue() {
      if (this.disposed) throw new Error("Model is disposed!");
      return this.value;
    }
    getLanguageId() {
      return "markdown";
    }
    onDidChangeContent() {
      return { dispose() {} };
    }
    dispose() {
      this.disposed = true;
    }
  }
  return {
    languageForPath: () => "markdown",
    monaco: {
      editor: {
        create: () => ({
          onDidChangeModelContent: () => ({ dispose() {} }),
          getModel: () => null,
          setModel: () => {},
          updateOptions: () => {},
          revealLineInCenterIfOutsideViewport: () => {},
          setPosition: () => {},
          focus: () => {},
          dispose: () => {},
        }),
        createModel: () => new FakeModel(() => {}),
        setModelLanguage: () => {},
      },
      languages: {
        registerCompletionItemProvider: () => ({ dispose() {} }),
        CompletionItemKind: { Reference: 17 },
      },
    },
  };
});

const META: NoteMetaDto = {
  path: "a.md",
  title: "甲笔记",
  tags: [],
  mtime_ms: 1700000000000,
  size: 10,
};

const CANVAS: CanvasDocDto = {
  version: 1,
  nodes: [
    { id: "n1", kind: "sticky", x: 10, y: 10, w: 180, h: 90, text: "便签一" },
    { id: "n2", kind: "sticky", x: 300, y: 200, w: 180, h: 90, text: "便签二" },
  ],
  edges: [
    { id: "e1", from: "n1", to: "n2" },
    { id: "e2", from: "n2", to: "n1" },
  ],
};

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function flush() {
  await act(async () => {});
}

async function pointerClick(el: HTMLElement) {
  // Fluent v9 下拉只在真实指针序列下展开（同 T-B7-3 configPrefill 先例）
  for (const type of ["pointerdown", "mousedown", "click"] as const) {
    await act(async () => {
      el.dispatchEvent(new MouseEvent(type, { bubbles: true }));
    });
  }
  await act(async () => {
    await new Promise((r) => setTimeout(r, 5));
  });
}

async function mountOnCanvas() {
  await act(async () => {
    root = createRoot(container);
    root.render(<NotesPanel />);
  });
  await flush();
  await click(buttonByText("画布")!);
  await flush();
  // 目录下拉选「（库根）」触发 loadCanvas("")
  const trigger = [...document.querySelectorAll<HTMLElement>('[role="combobox"]')].find((t) =>
    t.textContent?.includes("（库根）"),
  );
  if (!trigger) throw new Error("缺画布目录下拉触发钮");
  await pointerClick(trigger);
  const opt = [...document.querySelectorAll<HTMLElement>(".fui-Option")].find((o) =>
    o.textContent?.includes("（库根）"),
  );
  if (!opt) throw new Error("下拉未展开或缺（库根）选项");
  await pointerClick(opt);
  await flush();
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(notesList).mockResolvedValue([META]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesReviewStats).mockResolvedValue({
    total: 0,
    due_today: 0,
    by_bucket: [0, 0, 0, 0],
    streak_days: 0,
  });
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesCanvasDirs).mockResolvedValue([""]);
  vi.mocked(notesCanvasGet).mockResolvedValue(structuredClone(CANVAS));
  vi.mocked(notesSync).mockResolvedValue({ added: 0, updated: 0, removed: 0, total: 1 });
  vi.mocked(notesRead).mockResolvedValue({ content: "# 甲", meta: META });
  vi.mocked(notesLinks).mockResolvedValue([]);
  vi.mocked(notesBacklinks).mockResolvedValue([]);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  vi.clearAllMocks();
});

describe("画布纯编辑口（T-B7-24 canvasEdit）", () => {
  it("deleteEdge/setNodeText 纯函数：原 doc 零变更、只动目标维度", () => {
    const after = deleteEdge(CANVAS, "e1");
    expect(after.nodes).toBe(CANVAS.nodes); // 节点数组原样携带
    expect(after.edges.map((e) => e.id)).toEqual(["e2"]);
    const edited = setNodeText(CANVAS, "n1", "改后");
    expect(edited.nodes.find((n) => n.id === "n1")?.text).toBe("改后");
    expect(edited.nodes.find((n) => n.id === "n2")?.text).toBe("便签二");
    expect(CANVAS.nodes.find((n) => n.id === "n1")?.text).toBe("便签一"); // 纯度
    // image src 归一：URI 原样、本地路径走注入的 convert（convertFileSrc 的测试替身）
    expect(canvasImgSrc("data:image/png;base64,x", () => "nope")).toBe("data:image/png;base64,x");
    expect(canvasImgSrc("C:/pics/a.png", (p) => `asset://${p}`)).toBe("asset://C:/pics/a.png");
  });
});

describe("NotesPanel 画布就地编辑与单边删除（T-B7-24）", () => {
  it("canvas_edgeDelete_keepsNodes：点选边→仅删选中边→写盘载荷节点数不变", async () => {
    await mountOnCanvas();
    expect(notesCanvasGet).toHaveBeenCalledWith(""); // 包装函数层=位置参数（IPC 案形转换在真 client 内）
    // 命中腿=每条边第二根透明线（stroke=transparent）
    const hit = [...container.querySelectorAll("line")].find((l) => l.getAttribute("stroke") === "transparent");
    expect(hit).toBeTruthy();
    await act(async () => {
      hit!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    const del = buttonByText("仅删选中边")!;
    expect(del.disabled).toBe(false); // 选中后解禁（未选中的 disabled 判据见下条用例）
    await click(del);
    await flush();
    expect(notesCanvasSave).toHaveBeenCalledTimes(1);
    const [, payload] = vi.mocked(notesCanvasSave).mock.calls[0];
    expect(payload.nodes.length).toBe(CANVAS.nodes.length); // 节点一个不少
    expect(payload.edges.map((e) => e.id)).toEqual(["e2"]);
  });

  it("canvas_edgeButton_disabledWithoutSelection：未选边时删边钮禁着（防误触全表扫描）", async () => {
    await mountOnCanvas();
    const del = buttonByText("仅删选中边")!;
    expect(del.disabled).toBe(true);
    await click(del);
    expect(notesCanvasSave).not.toHaveBeenCalled();
  });

  it("canvas_nodeTextEdit_savesViaExistingCommand：双击节点→浮层改文→保存走 notes_canvas_save", async () => {
    await mountOnCanvas();
    const node = [...container.querySelectorAll("div")].find((d) => d.textContent === "便签一");
    expect(node).toBeTruthy();
    await act(async () => {
      node!.dispatchEvent(new MouseEvent("dblclick", { bubbles: true }));
    });
    const ta = container.querySelector<HTMLTextAreaElement>("[data-node-text-editor]");
    expect(ta).toBeTruthy();
    expect(ta!.value).toBe("便签一"); // 初值=节点现文
    const setter = Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, "value")!.set!;
    await act(async () => {
      setter.call(ta, "就地改文");
      ta!.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click(buttonByText("保存文本")!);
    await flush();
    expect(notesCanvasSave).toHaveBeenCalledTimes(1);
    const [, payload] = vi.mocked(notesCanvasSave).mock.calls[0];
    expect(payload.nodes.find((n) => n.id === "n1")?.text).toBe("就地改文");
    expect(payload.edges.length).toBe(CANVAS.edges.length); // 改文本不动边
  });
});

describe("NotesPanel 复习统计卡（T-B7-24）", () => {
  it("notesPanel_statsCard_rendersBuckets：四档计数+今日到期+连续天数上屏", async () => {
    vi.mocked(notesReviewStats).mockResolvedValue({
      total: 9,
      due_today: 4,
      by_bucket: [2, 3, 1, 3],
      streak_days: 7,
    });
    await act(async () => {
      root = createRoot(container);
      root.render(<NotesPanel />);
    });
    await flush();
    await click(buttonByText("复习（0 到期）")!);
    await flush();
    const card = container.querySelector("[data-stats-card]");
    expect(card).toBeTruthy();
    expect(card!.textContent).toContain("总 9 张");
    expect(card!.textContent).toContain("今日到期 4");
    expect(card!.textContent).toContain("连续 7 天");
    expect(card!.textContent).toContain("新卡 2");
    expect(card!.textContent).toContain("年幼 3");
    expect(card!.textContent).toContain("中年 1");
    expect(card!.textContent).toContain("成熟 3");
  });

  it("statsCard_absentWhenStatsNull：统计腿失败不连坐队列（分腿裁决的活体负例）", async () => {
    vi.mocked(notesReviewStats).mockRejectedValue(new Error("boom"));
    vi.mocked(notesReviewQueue).mockResolvedValue([
      { id: "c1", note_path: null, front: "正", back: "背", ef: 2.5, interval_days: 1, reps: 1, due_ms: 0 },
    ]);
    await act(async () => {
      root = createRoot(container);
      root.render(<NotesPanel />);
    });
    await flush();
    await click(buttonByText("复习（1 到期）")!);
    await flush();
    expect(container.querySelector("[data-stats-card]")).toBeNull();
    expect(container.textContent).toContain("正"); // 队列照常渲染
  });
});
