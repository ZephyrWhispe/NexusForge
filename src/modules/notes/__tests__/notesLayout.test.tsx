import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import NotesPanel from "../NotesPanel";
import notesPanelSrc from "../NotesPanel.tsx?raw";
import { SUBNAV } from "../../../layout/modules";
import { isNotesTab, useSession } from "../../../stores/session";
import {
  notesByTag,
  notesCanvasDirs,
  notesCanvasGet,
  notesCards,
  notesList,
  notesReviewQueue,
  notesReviewStats,
  notesSearch,
  notesSync,
  type NoteCardDto,
  type NoteMetaDto,
} from "../../../ipc/client";

// D-43 C7：笔记面板重排的判据面。四件事各钉一枚——
// ① 左轨注册表与面板视图门控对合（SUBNAV.notes 三枚 id 必须全是 NotesTab，否则左轨出现
//    点了没反应的撒谎按钮）；三视图互斥⇒view 形制（scope:"anchor" 会让点了不切换还留着上一视图）；
// ② 每视图恰好一枚吸顶工具条，主按钮恒末位（规范 5 节）；
// ③ 内层 480px 视口撤除后，长清单/卡片靠分页显影，页脚报"共 n · 在场 k"而非静默截断；
//    画布「引用笔记」下拉的 50 条上限同理落在页脚；
// ④ 行内标签的前 3 枚截断给出口（角标 +N 的 title 带余下名单）。
// 像素/滚动一律不在此证明（jsdom 无布局引擎），归 C9 真机 CDP 走查。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    notesList: vi.fn(),
    notesSearch: vi.fn(),
    notesByTag: vi.fn(),
    notesReviewQueue: vi.fn(),
    notesCards: vi.fn(),
    notesReviewStats: vi.fn(),
    notesCanvasDirs: vi.fn(),
    notesCanvasGet: vi.fn(),
    notesSync: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

// 本档不进编辑态（不点任何笔记行），轻量桩即可——形同 backendSearch 的在册约定
vi.mock("../../../monaco/setup", () => ({
  languageForPath: () => "markdown",
  monaco: {
    editor: { create: vi.fn(), createModel: vi.fn() },
    languages: { registerCompletionItemProvider: vi.fn(), CompletionItemKind: { Reference: 17 } },
  },
}));

const meta = (i: number, tags: string[] = []): NoteMetaDto => ({
  path: `n${i}.md`,
  title: `N${i}`,
  tags,
  mtime_ms: 1700000000000,
  size: 10,
});

const card = (i: number): NoteCardDto => ({
  id: `c${i}`,
  note_path: null,
  front: `F${i}`,
  back: "B",
  ef: 2.5,
  interval_days: 4,
  reps: 1,
  due_ms: 1700000000000,
});

/** 视图 id → 该视图工具条的主按钮文本（末位判据） */
const PRIMARY: Record<string, string> = {
  notes: "新建",
  review: "新建卡片",
  canvas: "保存画布",
};

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function switchTab(id: string) {
  await act(async () => {
    useSession.getState().setNotesTab(id);
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<NotesPanel />);
  });
  // 挂载腿四路并发（列表／复习／画布目录／进面板自动同步）：排空两拍微任务，
  // 否则 setAll 落在测试体之外＝act 噪音（与判据无关）
  await act(async () => {});
  await act(async () => {});
}

/** 区块根（Section 自带 data-nf="sec"），标题＝head 行首个 Text（span），不含操作区文案 */
function sectionTitles(): string[] {
  return [...document.querySelectorAll('[data-nf="sec"]')].map(
    (el) => el.querySelector("span")?.textContent?.trim() ?? "",
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(notesList).mockResolvedValue([meta(0)]);
  vi.mocked(notesSearch).mockResolvedValue([]);
  vi.mocked(notesByTag).mockResolvedValue([]);
  vi.mocked(notesReviewQueue).mockResolvedValue([]);
  vi.mocked(notesCards).mockResolvedValue([]);
  vi.mocked(notesReviewStats).mockResolvedValue({
    total: 0,
    due_today: 0,
    by_bucket: [0, 0, 0, 0],
    streak_days: 0,
  });
  vi.mocked(notesCanvasDirs).mockResolvedValue([""]);
  vi.mocked(notesCanvasGet).mockResolvedValue({ version: 1, nodes: [], edges: [] });
  vi.mocked(notesSync).mockResolvedValue({ added: 0, updated: 0, removed: 0, total: 1 });
  act(() => useSession.getState().setNotesTab("notes"));
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

describe("NotesPanel 重排（D-43 C7）", () => {
  it("notesLayout_registryCoversGatedViews_andMutualExclusionHolds", async () => {
    const items = SUBNAV.notes.flatMap((s) => s.items);
    expect(items.map((i) => i.scope)).toEqual(["view", "view", "view"]);
    for (const i of items) expect(isNotesTab(i.id), `${i.id} 不是 NotesTab`).toBe(true);

    await mount();
    // 缺省档 notes：三列工作台的区块各有标题（首层无题＝"直接放在页面上"的形态学根因）
    expect(sectionTitles()).toEqual(["笔记清单（1）", "当前笔记", "大纲（0）"]);
    expect(document.body.textContent).not.toContain("今日队列");

    await switchTab("review");
    expect(sectionTitles()).toEqual(["复习统计", "今日队列（0 到期）", "全部卡片（0）"]);
    expect(
      document.querySelector('[placeholder="搜索标题/路径/标签/正文（后端全文索引）"]'),
      "笔记档不得留在 DOM",
    ).toBeNull();

    await switchTab("canvas");
    expect(sectionTitles()).toEqual(["画布（0 节点 / 0 连线）"]);
    expect(document.body.textContent).not.toContain("全部卡片");

    // 野值不落（持久化快照被改坏时确定性回落，不白屏）
    await switchTab("nope");
    expect(useSession.getState().notesTab).toBe("canvas");
  });

  it("notesLayout_oneToolbarPerView_withPrimaryLast", async () => {
    await mount();
    for (const tab of ["notes", "review", "canvas"]) {
      await switchTab(tab);
      const toolbars = container.querySelectorAll('[role="toolbar"]');
      expect(toolbars, `${tab} 应恰有一枚工具条`).toHaveLength(1);
      const buttons = [...toolbars[0]!.querySelectorAll("button")];
      expect(buttons[buttons.length - 1]?.textContent?.trim(), `${tab} 主按钮恒末位`).toBe(
        PRIMARY[tab],
      );
    }
    // 笔记档的可编辑字段在工具条内（名称输入是「新建」的前置项，与主按钮同区）
    await switchTab("notes");
    const bar = document.querySelector('[role="toolbar"]')!;
    expect(bar.querySelector('[placeholder="新笔记名或 sub/名称.md"]')).not.toBeNull();
    // 复习档主按钮在未选笔记时不带关联尾巴（尾巴是"这张卡挂在哪篇笔记上"的显影）
    await switchTab("review");
    expect(
      document
        .querySelector('[role="toolbar"]')!
        .querySelector('[placeholder="卡片背面（可空）"]'),
      "正反面输入应在工具条内",
    ).not.toBeNull();
  });

  it("notesLayout_listPagination_footerDisclosesTruncation", async () => {
    vi.mocked(notesList).mockResolvedValue(Array.from({ length: 201 }, (_, i) => meta(i)));
    await mount();
    // 首屏档＝100 行（内层 480px 视口撤除后的替代形制）
    expect(document.body.textContent).toContain("笔记清单（201）");
    expect(document.body.textContent).toContain("共 201 篇 · 在场 100 篇");
    expect(document.body.textContent).toContain("n99.md");
    expect(document.body.textContent, "超出首屏的行不得出现在场").not.toContain("n100.md");
    const more = buttonByText("显示更多");
    expect(more, "静默截断必须给出口").toBeDefined();
    await click(more!);
    // 游标按首屏档续加（100→200），不是"一次全放"——出口本身也分级
    expect(document.body.textContent).toContain("共 201 篇 · 在场 200 篇");
    expect(document.body.textContent).toContain("n100.md");
    expect(document.body.textContent).not.toContain("n200.md");
    await click(buttonByText("显示更多")!);
    expect(document.body.textContent).toContain("共 201 篇 · 在场 201 篇");
    expect(document.body.textContent).toContain("n200.md");
    expect(buttonByText("显示更多")).toBeUndefined();
  });

  it("notesLayout_cardAndCanvasRefPagination_areDisclosedInFooters", async () => {
    vi.mocked(notesCards).mockResolvedValue(Array.from({ length: 101 }, (_, i) => card(i)));
    vi.mocked(notesList).mockResolvedValue(Array.from({ length: 61 }, (_, i) => meta(i)));
    await mount();

    await switchTab("review");
    expect(document.body.textContent).toContain("全部卡片（101）");
    expect(document.body.textContent).toContain("共 101 张 · 在场 100 张");
    expect(document.body.textContent).not.toContain("F100");
    await click(buttonByText("显示更多")!);
    expect(document.body.textContent).toContain("共 101 张 · 在场 101 张");
    expect(document.body.textContent).toContain("F100");

    // 引用候选 50 条上限：下拉选项在 jsdom 里不展开，枚数走页脚显影
    await switchTab("canvas");
    expect(document.body.textContent).toContain("引用候选前 50 / 61 篇");
    // 页脚形状另有源码钉：上限取常量而非写死数字
    expect(notesPanelSrc).toContain("all.slice(0, CANVAS_REF_MAX)");
  });

  it("notesLayout_inlineTagTruncation_givesExitCount", async () => {
    vi.mocked(notesList).mockResolvedValue([meta(1, ["a", "b", "c", "d", "e"])]);
    await mount();
    expect(document.body.textContent).toContain("笔记清单（1）");
    const plus = [...document.querySelectorAll("[title]")].find((el) =>
      el.getAttribute("title")?.startsWith("另有 2 枚标签"),
    ) as HTMLElement | undefined;
    expect(plus, "前 3 枚截断必须给出口").toBeDefined();
    expect(plus!.textContent?.trim()).toBe("+2");
    expect(plus!.getAttribute("title")).toBe("另有 2 枚标签：d、e");
  });
});
